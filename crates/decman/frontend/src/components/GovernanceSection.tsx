import { useState, useEffect, useCallback, useMemo, useRef } from "react";
import {
  Autocomplete,
  Box,
  Typography,
  Button,
  Chip,
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
  Portal,
} from "@mui/material";
import ExpandMoreIcon from "@mui/icons-material/ExpandMore";
import ExpandLessIcon from "@mui/icons-material/ExpandLess";
import CheckCircleIcon from "@mui/icons-material/CheckCircle";
import AddIcon from "@mui/icons-material/Add";
import RefreshIcon from "@mui/icons-material/Refresh";
import {
  API_BASE,
  ADMIN_ACCESS,
  TEMPLATE_ALLOCATION_FACTORY,
  TEMPLATE_REGISTRAR_SERVICE,
} from "../constants";
import { authenticatedFetch } from "../api";
import { getActionTypeOptions } from "../governanceFormat";
import { fieldHelpAdornment, TextHelp } from "./FieldHelp";
import type {
  GovernanceResponse,
  ActionType,
  ConfirmActionRequest,
  AppRewardBeneficiary,
  Claim,
  ProviderServiceInfo,
  ProviderServicesResponse,
  UserServiceInfo,
  UserServicesResponse,
  CredentialOfferInfo,
  CredentialOffersResponse,
  CredentialInfo,
  CredentialsResponse,
  RegistrarServiceInfo,
  RegistrarServicesResponse,
  RegistrarServiceRequestInfo,
  RegistrarServiceRequestsResponse,
  ProviderConfigurationInfo,
  ProviderConfigurationsResponse,
  PartyCredentialRequirement,
  ContractWithBlob,
  ContractQueryResponse,
  Network,
  NetworkInfo,
  ProposeActionRequest,
  ProposalType,
  InstrumentAllowance,
  InstrumentInfo,
  InstrumentsResponse,
  TransferInstructionInfo,
  TransferInstructionsResponse,
  TokenRequestInfo,
  MintRequestsResponse,
  BurnRequestsResponse,
  TransferPreapprovalsResponse,
  TransferFactoryInfo,
  TransferFactoriesResponse,
  Holding,
  HoldingsResponse,
  GovernanceState,
  GovernanceStateResponse,
  ActiveCouponReassignmentDelegation,
  CouponReassignmentDelegationSummary,
} from "../types";

type ActionTypeKey = ActionType["type"];

interface GovernanceSectionProps {
  partyId: string;
  rulesContractId?: string;
  governanceContractIds?: string[];
  defaultOperatorParty?: string;
  network?: Network;
  governanceType?: "core_self" | "core_domain";
  /// Called after every successful mutating action (propose / confirm /
  /// execute / revoke / expire / domain confirm / domain execute) so the
  /// parent can refresh sibling views (e.g. the audit trail tab).
  onAfterAction?: () => void;
  /// Called after a domain proposal is successfully created. The hosting
  /// dialog wires this to its `onClose` so the modal disappears on success;
  /// fires after `onAfterAction` so refreshes still run.
  onProposalCreated?: () => void;
  /// Which half of the section to render:
  /// - "actions"   = governance-action confirmations + new-action form (default)
  /// - "proposals" = domain-proposal list + new-proposal form (core_self only)
  /// - undefined   = both (legacy, used when rendered inline on the party page)
  view?: "actions" | "proposals";
  /// When provided, the inline Submit Confirmation / Submit Proposal button
  /// is rendered into this DOM node (via `Portal`) instead of inline at the
  /// bottom of the form. Used by `GovernanceActionsDialog` to lift the
  /// primary action into its `DialogActions` footer next to Close.
  submitPortalEl?: HTMLElement | null;
}

/// Freely-transferable balance of a holding: total minus the locked
/// (escrowed) portion. A transfer can only be funded from unlocked holdings,
/// so the amount field is capped on this rather than `amount`.
const holdingAvailable = (h: Holding): number =>
  Number(h.amount) - Number(h.locked_amount);

/// Default validity window (hours) for a Transfer proposal / two-step offer.
/// Mirrors the backend default; overridable per-transfer in the form.
const DEFAULT_TRANSFER_EXPIRY_HOURS = 24;

export const GovernanceSection = ({
  partyId,
  rulesContractId: initialRulesContractId,
  governanceContractIds = [],
  defaultOperatorParty,
  network,
  governanceType = "core_self",
  onAfterAction,
  onProposalCreated,
  view,
  submitPortalEl,
}: GovernanceSectionProps) => {
  const showActionsHalf = view !== "proposals";
  const showProposalsHalf = view !== "actions";
  const [expanded, setExpanded] = useState(true);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [data, setData] = useState<GovernanceResponse | null>(null);
  // Domain proposal state
  // Auto-expand the form when this section is rendered in proposals-only mode
  // (header "New Proposal" button); otherwise start collapsed.
  const [showProposalForm, setShowProposalForm] = useState(
    view === "proposals",
  );
  const [proposalType, setProposalType] = useState<ProposalType["type"]>("setup_cc_preapproval");
  const [proposalProvider, setProposalProvider] = useState("");
  const [proposalExpectedDso, setProposalExpectedDso] = useState("");
  const [proposalDelegate, setProposalDelegate] = useState("");
  // datetime-local string; converted to micros-since-epoch on submit.
  const [proposalDelegationExpiresAt, setProposalDelegationExpiresAt] =
    useState("");
  const [proposalAmuletMergeLimit, setProposalAmuletMergeLimit] =
    useState("10");
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
  const [proposalAllocationFactoryCid, setProposalAllocationFactoryCid] = useState("");
  const [proposalRecipient, setProposalRecipient] = useState("");
  const [proposalHolder, setProposalHolder] = useState("");
  // Credential proposal state (offer_free / accept_free)
  const [proposalUserServiceCid, setProposalUserServiceCid] = useState("");
  const [proposalCredentialId, setProposalCredentialId] = useState("");
  const [proposalCredentialClaimsText, setProposalCredentialClaimsText] = useState("");
  const [proposalCredentialOfferCid, setProposalCredentialOfferCid] = useState("");
  // Accept holder-initiated mint/burn request state
  const [proposalMintRequestCid, setProposalMintRequestCid] = useState("");
  const [proposalBurnRequestCid, setProposalBurnRequestCid] = useState("");
  // Credential cids proving the request's holder meets the instrument's
  // issuer requirements. Passed into the accept's choice context.
  const [proposalIssuerCredentialCids, setProposalIssuerCredentialCids] = useState<string[]>([]);
  // Registrar-onboarding proposal state
  const [proposalRegistrarServiceRequestCid, setProposalRegistrarServiceRequestCid] = useState("");
  const [proposalProviderConfigurationCid, setProposalProviderConfigurationCid] = useState("");
  // Requirement-editor rows: an issuer party id plus its required claims as
  // "property,value" lines. Parsed into PartyCredentialRequirement on submit.
  const [proposalRegistrarRequirements, setProposalRegistrarRequirements] = useState<
    { issuer: string; claimsText: string }[]
  >([]);
  const [proposalHolderRequirements, setProposalHolderRequirements] = useState<
    { issuer: string; claimsText: string }[]
  >([]);
  const [proposalIssuerRequirements, setProposalIssuerRequirements] = useState<
    { issuer: string; claimsText: string }[]
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
  const [proposalLoading, setProposalLoading] = useState(false);
  const [rulesContractId, setRulesContractId] = useState(
    initialRulesContractId || "",
  );

  // Action form state
  // Auto-expand the action form when the section is rendered in actions-only
  // mode (pencil icon → modal); otherwise start collapsed.
  const [showNewActionForm, setShowNewActionForm] = useState(
    view === "actions",
  );
  const [selectedActionType, setSelectedActionType] = useState<ActionTypeKey>(
    governanceType === "core_self"
      ? "governance_add_member"
      : "utility_create_provider_request",
  );
  const [formLoading, setFormLoading] = useState(false);

  // Form fields for various action types
  const [memberParty, setMemberParty] = useState("");
  const [newThreshold, setNewThreshold] = useState(2);
  const [timeoutMicroseconds, setTimeoutMicroseconds] = useState(3600000000);
  // Latest applied governance values (threshold from /governance/confirmations,
  // timeout from /governance/state). Used to prefill the new-action form so it
  // opens with the current values, not hardcoded 2 / 1h.
  const [governanceState, setGovernanceState] =
    useState<GovernanceState | null>(null);
  // Once the user types into a threshold/timeout field we stop auto-seeding
  // from server state — otherwise the 10s poll would clobber their input.
  // `resetActionForm` flips these back to false so the next form opening
  // re-seeds from the latest applied values.
  const userEditedThresholdRef = useRef(false);
  const userEditedTimeoutRef = useRef(false);
  // New fields for additional action types
  const [operatorParty, setOperatorParty] = useState(
    defaultOperatorParty || "",
  );
  // Sync the autofetched operator party (from App.tsx) into both the action
  // and proposal operator states once it arrives — without this, the fields
  // stay empty whenever the fetch completes after this component has already
  // mounted with an empty default.
  useEffect(() => {
    if (defaultOperatorParty) {
      setOperatorParty(defaultOperatorParty);
      setProposalOperator(defaultOperatorParty);
    }
  }, [defaultOperatorParty]);
  const [providerServiceCid, setProviderServiceCid] = useState("");
  const [userServiceCid, setUserServiceCid] = useState("");
  const [amuletRulesCid, setAmuletRulesCid] = useState("");
  const [dsoPartyId, setDsoPartyId] = useState("");

  // Utility onboarding fields
  const [holderServiceRequestCid, setHolderServiceRequestCid] = useState("");
  const [holderParty, setHolderParty] = useState("");

  // Credential fields
  const [credentialId, setCredentialId] = useState("");
  const [credentialDescription, setCredentialDescription] = useState("");
  const [credentialOfferCid, setCredentialOfferCid] = useState("");
  const [claims, setClaims] = useState<Claim[]>([]);

  // Available services from ACS
  const [providerServices, setProviderServices] = useState<
    ProviderServiceInfo[]
  >([]);
  const [userServices, setUserServices] = useState<UserServiceInfo[]>([]);
  // Pending CredentialOffer contracts visible to this party. Powers the
  // CredentialOffer dropdowns on the Accept Free Credential forms.
  const [credentialOffers, setCredentialOffers] = useState<CredentialOfferInfo[]>([]);
  const [credentialOffersLoading, setCredentialOffersLoading] = useState(false);
  const [registrarServiceContracts, setRegistrarServiceContracts] = useState<ContractWithBlob[]>([]);
  // Typed `MintRequest`/`BurnRequest` rows so the Accept dropdowns can show
  // holder → amount instrument (…cid) — mirroring the Accept Transfer UX —
  // instead of just the contract id slug.
  const [mintRequestContracts, setMintRequestContracts] = useState<TokenRequestInfo[]>([]);
  const [burnRequestContracts, setBurnRequestContracts] = useState<TokenRequestInfo[]>([]);
  const [mintRequestsLoading, setMintRequestsLoading] = useState(false);
  const [burnRequestsLoading, setBurnRequestsLoading] = useState(false);
  // `Credential` contracts visible to this party. Powers the issuer
  // credential picker on the Accept Mint/Burn Request forms.
  const [availableCredentials, setAvailableCredentials] = useState<CredentialInfo[]>([]);
  const [credentialsLoading, setCredentialsLoading] = useState(false);
  // Pending `RegistrarServiceRequest` and `ProviderConfiguration` contracts.
  // Power the pickers on the Onboard Registrar form.
  const [registrarServiceRequests, setRegistrarServiceRequests] = useState<RegistrarServiceRequestInfo[]>([]);
  const [registrarServiceRequestsLoading, setRegistrarServiceRequestsLoading] = useState(false);
  // Typed `RegistrarService` contracts. Power the Provision Instrument
  // picker, which needs the registrar field to filter out services this
  // decparty co-signed as the provider.
  const [availableRegistrarServices, setAvailableRegistrarServices] = useState<RegistrarServiceInfo[]>([]);
  const [registrarServicesLoading, setRegistrarServicesLoading] = useState(false);
  const [providerConfigurations, setProviderConfigurations] = useState<ProviderConfigurationInfo[]>([]);
  const [providerConfigurationsLoading, setProviderConfigurationsLoading] = useState(false);
  // InstrumentConfiguration contracts fetched from /instruments. Each one
  // represents a token the governance party can mint/burn against and exposes
  // its parsed instrument_admin + instrument_id, so we can drive a real
  // dropdown without the frontend having to decode contract blobs.
  const [availableInstruments, setAvailableInstruments] = useState<InstrumentInfo[]>([]);
  const [instrumentsLoading, setInstrumentsLoading] = useState(false);
  // Open TransferInstruction contracts addressed to this dec-party. Powers the
  // Accept Transfer proposal dropdown.
  const [openTransferInstructions, setOpenTransferInstructions] = useState<
    TransferInstructionInfo[]
  >([]);
  const [transferInstructionsLoading, setTransferInstructionsLoading] = useState(false);
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

  // Contracts fetched by template (with blobs)
  const [allocationFactoryContracts, setAllocationFactoryContracts] = useState<ContractWithBlob[]>([]);
  const [amuletRulesLoading, setAmuletRulesLoading] = useState(false);

  // Burn Mint Factory (from external API, used for processor deployment)

  // Update rulesContractId when prop changes
  useEffect(() => {
    if (initialRulesContractId) {
      setRulesContractId(initialRulesContractId);
    }
  }, [initialRulesContractId]);

  const fetchGovernance = useCallback(async () => {
    try {
      const res = await authenticatedFetch(
        `${API_BASE}/governance/confirmations?party_id=${encodeURIComponent(partyId)}`,
      );
      if (res.ok) {
        const response: GovernanceResponse = await res.json();
        setData(response);
        if (response.rules_contract_id) {
          setRulesContractId(response.rules_contract_id);
        }
        setError(null);
      } else {
        const errData = await res.json().catch(() => ({}));
        setError(errData.error || "Failed to fetch governance data");
      }
    } catch (e) {
      setError(
        e instanceof Error ? e.message : "Failed to fetch governance data",
      );
    } finally {
      setLoading(false);
    }
  }, [partyId]);

  useEffect(() => {
    fetchGovernance();
    const interval = setInterval(fetchGovernance, 10000); // Poll every 10 seconds
    return () => clearInterval(interval);
  }, [fetchGovernance]);

  // Fetch governance state for action_confirmation_timeout_microseconds.
  // Threshold also comes back here, but the form already uses the threshold
  // from `data` (the confirmations payload) — `governanceState` is primarily
  // for the timeout field's prefill.
  const fetchGovernanceStateForPrefill = useCallback(async () => {
    try {
      const res = await authenticatedFetch(
        `${API_BASE}/governance/state?party_id=${encodeURIComponent(partyId)}`,
      );
      if (!res.ok) return;
      const body: GovernanceStateResponse = await res.json();
      setGovernanceState(body.state ?? null);
    } catch {
      /* fall back to hardcoded defaults */
    }
  }, [partyId]);

  useEffect(() => {
    fetchGovernanceStateForPrefill();
  }, [fetchGovernanceStateForPrefill]);

  // Seed `newThreshold` from the active GovernanceRules contract once state
  // arrives. NOTE: do not use `data.threshold` here — that field on the
  // `/governance/confirmations` response is the decentralized-namespace
  // topology threshold (e.g. 2-of-3 owners), not the governance-rules
  // threshold. They are usually different numbers. The ref guard prevents
  // polling/refreshes from clobbering the user's typed value mid-edit.
  useEffect(() => {
    if (
      governanceState?.threshold != null &&
      !userEditedThresholdRef.current
    ) {
      setNewThreshold(Number(governanceState.threshold));
    }
  }, [governanceState?.threshold]);

  // Same pattern for the action confirmation timeout.
  useEffect(() => {
    const us = governanceState?.action_confirmation_timeout_microseconds;
    if (us != null && !userEditedTimeoutRef.current) {
      setTimeoutMicroseconds(us);
    }
  }, [governanceState?.action_confirmation_timeout_microseconds]);

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
        // Auto-select first provider service
        if (response.services.length > 0) {
          setProviderServiceCid(response.services[0].contract_id);
        }
      }

      if (userRes.ok) {
        const response: UserServicesResponse = await userRes.json();
        setUserServices(response.services);
        // Auto-select first user service
        if (response.services.length > 0) {
          setUserServiceCid(response.services[0].contract_id);
        }
      }
    } catch (e) {
      console.error("Failed to fetch services:", e);
    } finally {
      setServicesLoading(false);
    }
  }, [partyId]);

  // Fetch services when action type needs service selection
  useEffect(() => {
    if (
      selectedActionType === "utility_setup" ||
      selectedActionType === "utility_accept_holder_service_request" ||
      selectedActionType === "credential_offer_free" ||
      selectedActionType === "credential_accept_free"
    ) {
      fetchServices();
    }
  }, [selectedActionType, fetchServices]);

  // Also fetch services when a proposal type that creates a service-request is
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
      proposalType === "create_provider_configuration" ||
      proposalType === "onboard_registrar"
    ) {
      fetchServices();
    }
  }, [proposalType, fetchServices]);

  // Fetch pending `CredentialOffer` contracts so the Accept Free Credential
  // forms can offer a dropdown instead of a hand-pasted contract id.
  const fetchCredentialOffers = useCallback(async () => {
    setCredentialOffersLoading(true);
    try {
      const res = await authenticatedFetch(
        `${API_BASE}/credential-offers?party_id=${encodeURIComponent(partyId)}`,
      );
      if (res.ok) {
        const response: CredentialOffersResponse = await res.json();
        setCredentialOffers(response.credential_offers);
      }
    } catch (e) {
      console.error("Failed to fetch credential offers:", e);
    } finally {
      setCredentialOffersLoading(false);
    }
  }, [partyId]);

  // Fetch `Credential` contracts so the Accept Mint/Burn Request forms can
  // offer an issuer-credential picker instead of hand-pasted contract ids.
  const fetchCredentials = useCallback(async () => {
    setCredentialsLoading(true);
    try {
      const res = await authenticatedFetch(
        `${API_BASE}/credentials?party_id=${encodeURIComponent(partyId)}`,
      );
      if (res.ok) {
        const response: CredentialsResponse = await res.json();
        setAvailableCredentials(response.credentials);
      }
    } catch (e) {
      console.error("Failed to fetch credentials:", e);
    } finally {
      setCredentialsLoading(false);
    }
  }, [partyId]);

  // Fetch pending `RegistrarServiceRequest` contracts so the Onboard
  // Registrar form can offer a picker instead of a hand-pasted contract id.
  const fetchRegistrarServiceRequests = useCallback(async () => {
    setRegistrarServiceRequestsLoading(true);
    try {
      const res = await authenticatedFetch(
        `${API_BASE}/registrar-service-requests?party_id=${encodeURIComponent(partyId)}`,
      );
      if (res.ok) {
        const response: RegistrarServiceRequestsResponse = await res.json();
        setRegistrarServiceRequests(response.registrar_service_requests);
      }
    } catch (e) {
      console.error("Failed to fetch registrar service requests:", e);
    } finally {
      setRegistrarServiceRequestsLoading(false);
    }
  }, [partyId]);

  // Fetch `ProviderConfiguration` contracts so the Onboard Registrar form
  // can offer a picker instead of a hand-pasted contract id.
  const fetchProviderConfigurations = useCallback(async () => {
    setProviderConfigurationsLoading(true);
    try {
      const res = await authenticatedFetch(
        `${API_BASE}/provider-configurations?party_id=${encodeURIComponent(partyId)}`,
      );
      if (res.ok) {
        const response: ProviderConfigurationsResponse = await res.json();
        setProviderConfigurations(response.provider_configurations);
      }
    } catch (e) {
      console.error("Failed to fetch provider configurations:", e);
    } finally {
      setProviderConfigurationsLoading(false);
    }
  }, [partyId]);

  // Fetch typed `RegistrarService` contracts so the Provision Instrument
  // picker can filter on the registrar field.
  const fetchRegistrarServices = useCallback(async () => {
    setRegistrarServicesLoading(true);
    try {
      const res = await authenticatedFetch(
        `${API_BASE}/services/registrar?party_id=${encodeURIComponent(partyId)}`,
      );
      if (res.ok) {
        const response: RegistrarServicesResponse = await res.json();
        setAvailableRegistrarServices(response.services);
      }
    } catch (e) {
      console.error("Failed to fetch registrar services:", e);
    } finally {
      setRegistrarServicesLoading(false);
    }
  }, [partyId]);

  useEffect(() => {
    if (
      selectedActionType === "credential_accept_free" ||
      proposalType === "accept_free_credential"
    ) {
      fetchCredentialOffers();
    }
  }, [selectedActionType, proposalType, fetchCredentialOffers]);

  // Offers this party can take via the Free direction: it is the holder and
  // the offer carries no billing params (`CredentialOffer_AcceptFree` rejects
  // billed offers).
  const acceptableCredentialOffers = useMemo(
    () => credentialOffers.filter((o) => o.is_free && o.holder === partyId),
    [credentialOffers, partyId],
  );

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

  // Prefill the credential proposal form's UserService once the list arrives —
  // parties typically have exactly one.
  useEffect(() => {
    if (
      (proposalType === "offer_free_credential" ||
        proposalType === "accept_free_credential") &&
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
    if (selectedActionType === "credential_accept_free" && !credentialOfferCid) {
      setCredentialOfferCid(offerCid);
    }
  }, [
    proposalType,
    selectedActionType,
    acceptableCredentialOffers,
    proposalCredentialOfferCid,
    credentialOfferCid,
  ]);

  // CredentialOffer picker shared by the direct Accept Free Credential action
  // and the Accept Free Credential proposal form. freeSolo keeps hand-pasting
  // a cid possible when the offer isn't visible to this participant.
  const renderCredentialOfferAutocomplete = (
    value: string,
    setValue: (v: string) => void,
  ) => (
    <Autocomplete
      size="small"
      freeSolo
      options={acceptableCredentialOffers}
      value={value}
      loading={credentialOffersLoading}
      onChange={(_event, newValue) => {
        if (typeof newValue === "string" || newValue === null) {
          setValue(newValue ?? "");
        } else {
          setValue(newValue.contract_id);
        }
      }}
      onInputChange={(_event, newValue, reason) => {
        // Keep the field in sync when the user types a cid by hand
        // (freeSolo fallback). `reset` fires when an option is selected;
        // we already handled that via `onChange`.
        if (reason === "input") {
          setValue(newValue);
        }
      }}
      getOptionLabel={(option) =>
        typeof option === "string" ? option : option.contract_id
      }
      isOptionEqualToValue={(option, val) =>
        typeof val === "string"
          ? option.contract_id === val
          : option.contract_id === val.contract_id
      }
      renderOption={(props, option) => {
        if (typeof option === "string") {
          return <li {...props}>{option}</li>;
        }
        const issuerName = option.issuer.split("::")[0];
        const cidTail = option.contract_id.slice(-8);
        return (
          <li {...props} key={option.contract_id}>
            <Box sx={{ display: "flex", flexDirection: "column", gap: 0.25 }}>
              <Typography variant="body2">
                {option.credential_id} — {issuerName} (…{cidTail})
              </Typography>
              {option.description && (
                <Typography variant="caption" color="text.secondary">
                  {option.description}
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
            <TextHelp text="Contract id of the pending CredentialOffer to accept.">
              CredentialOffer Contract ID
            </TextHelp>
          }
          required
          helperText={
            credentialOffersLoading
              ? "Loading pending offers…"
              : acceptableCredentialOffers.length === 0
                ? "No pending free offers visible — paste a contract id directly if you have one"
                : "Pick a pending offer, or paste a contract id"
          }
        />
      )}
    />
  );

  // Requirement-list editor shared by the Create Provider Configuration and
  // Provision Instrument forms. Each row is an issuer party id plus the
  // claims a credential from that issuer must carry, one "property,value"
  // per line. Mirrors the beneficiaries row editor.
  const renderRequirementRows = (
    label: string,
    help: string,
    rows: { issuer: string; claimsText: string }[],
    setRows: (rows: { issuer: string; claimsText: string }[]) => void,
  ) => (
    <>
      <Typography variant="caption" color="text.secondary" sx={{ display: "block" }}>
        <TextHelp text={help}>{label} (add issuer + required claims)</TextHelp>
      </Typography>
      {rows.map((row, idx) => (
        <Box key={idx} sx={{ display: "flex", gap: 1, mb: 1, alignItems: "flex-start" }}>
          <TextField
            label="Issuer Party"
            value={row.issuer}
            onChange={(e) => {
              const updated = [...rows];
              updated[idx] = { ...row, issuer: e.target.value };
              setRows(updated);
            }}
            size="small"
            sx={{ flex: 2 }}
            slotProps={{
              input: {
                endAdornment: fieldHelpAdornment(
                  "Party id whose credential satisfies this requirement. The governance party mints the credential itself when it is the issuer; other issuers must credential the party out-of-band.",
                  "Help for Issuer Party",
                ),
              },
            }}
          />
          <TextField
            label="Required claims (one per line: property,value)"
            value={row.claimsText}
            onChange={(e) => {
              const updated = [...rows];
              updated[idx] = { ...row, claimsText: e.target.value };
              setRows(updated);
            }}
            size="small"
            sx={{ flex: 3 }}
            multiline
            minRows={1}
            maxRows={4}
            slotProps={{
              input: {
                endAdornment: fieldHelpAdornment(
                  'Claims the issuer\'s credential must contain. One per line, each formatted as "property,value" (e.g. role,registrar).',
                  "Help for Required Claims",
                ),
              },
            }}
          />
          <Button
            size="small"
            color="error"
            onClick={() => setRows(rows.filter((_, i) => i !== idx))}
          >
            Remove
          </Button>
        </Box>
      ))}
      <Box sx={{ display: "flex", alignItems: "center", gap: 2 }}>
        <Button
          size="small"
          onClick={() => setRows([...rows, { issuer: partyId, claimsText: "" }])}
        >
          Add Requirement
        </Button>
      </Box>
    </>
  );

  // Fetch InstrumentConfiguration contracts (a.k.a. "our tokens"). Used by
  // Mint/Burn (for instrument_id + instrument_configuration_cid) and by
  // set_provider_app_reward_beneficiaries (for instrument_configuration_cid).
  const fetchInstruments = useCallback(async () => {
    setInstrumentsLoading(true);
    try {
      const res = await authenticatedFetch(
        `${API_BASE}/instruments?party_id=${encodeURIComponent(partyId)}`,
      );
      if (res.ok) {
        const response: InstrumentsResponse = await res.json();
        setAvailableInstruments(response.instruments);
      }
    } catch (e) {
      console.error("Failed to fetch instruments:", e);
    } finally {
      setInstrumentsLoading(false);
    }
  }, [partyId]);

  // Fetch instruments when a proposal type needs them
  useEffect(() => {
    if (
      proposalType === "mint" ||
      proposalType === "burn" ||
      proposalType === "accept_mint_request" ||
      proposalType === "accept_burn_request" ||
      proposalType === "set_provider_app_reward_beneficiaries" ||
      proposalType === "onboard_instrument_issuers"
    ) {
      fetchInstruments();
    }
  }, [proposalType, fetchInstruments]);

  // Fetch open `TransferInstruction` contracts for the Accept Transfer
  // proposal dropdown so operators can pick a transfer offer instead of
  // pasting a contract id.
  const fetchOpenTransferInstructions = useCallback(async () => {
    setTransferInstructionsLoading(true);
    try {
      const res = await authenticatedFetch(
        `${API_BASE}/governance/transfer-instructions?party_id=${encodeURIComponent(partyId)}`,
      );
      if (res.ok) {
        const response: TransferInstructionsResponse = await res.json();
        setOpenTransferInstructions(response.transfer_instructions);
      }
    } catch (e) {
      console.error("Failed to fetch transfer instructions:", e);
    } finally {
      setTransferInstructionsLoading(false);
    }
  }, [partyId]);

  useEffect(() => {
    if (proposalType === "accept_transfer") {
      fetchOpenTransferInstructions();
    }
  }, [proposalType, fetchOpenTransferInstructions]);

  // Pull typed open mint/burn requests for the Accept dropdowns. Mirrors the
  // Accept Transfer flow above — the backend extracts holder/amount/instrument
  // from the contract payload so we can render a useful label.
  const fetchOpenMintRequests = useCallback(async () => {
    setMintRequestsLoading(true);
    try {
      const res = await authenticatedFetch(
        `${API_BASE}/governance/mint-requests?party_id=${encodeURIComponent(partyId)}`,
      );
      if (res.ok) {
        const response: MintRequestsResponse = await res.json();
        setMintRequestContracts(response.mint_requests);
      }
    } catch (e) {
      console.error("Failed to fetch mint requests:", e);
    } finally {
      setMintRequestsLoading(false);
    }
  }, [partyId]);

  const fetchOpenBurnRequests = useCallback(async () => {
    setBurnRequestsLoading(true);
    try {
      const res = await authenticatedFetch(
        `${API_BASE}/governance/burn-requests?party_id=${encodeURIComponent(partyId)}`,
      );
      if (res.ok) {
        const response: BurnRequestsResponse = await res.json();
        setBurnRequestContracts(response.burn_requests);
      }
    } catch (e) {
      console.error("Failed to fetch burn requests:", e);
    } finally {
      setBurnRequestsLoading(false);
    }
  }, [partyId]);

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

  // Mint/Burn always use the decparty as the instrument admin — seed the field
  // unconditionally so it's populated even before (or without) an Instrument
  // selection from the dropdown. NOT applied to setup_token_preapproval or
  // transfer because those can target foreign-issued instruments where the
  // admin is a different party.
  useEffect(() => {
    if (proposalType === "mint" || proposalType === "burn") {
      setProposalInstrumentIdAdmin(partyId);
    }
  }, [proposalType, partyId]);

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
      options?: { activeOnly?: boolean },
    ) => {
      const params = new URLSearchParams({
        party_id: partyId,
        package_id: template.package_ref,
        module_name: template.module,
        entity_name: template.entity,
      });
      if (template.interface) params.set("interface", "true");
      if (options?.activeOnly) params.set("active_only", "true");
      const res = await authenticatedFetch(`${API_BASE}/contracts/query?${params}`);
      if (res.ok) {
        const data: ContractQueryResponse = await res.json();
        return data.contracts;
      }
      return [];
    },
    [partyId],
  );

  // Fetch AllocationFactory contracts when Mint/Burn proposal is selected.
  // (set_enable_result_contracts needs RegistrarService instead.)
  useEffect(() => {
    if (proposalType === "mint" || proposalType === "burn") {
      fetchContractsByTemplate(TEMPLATE_ALLOCATION_FACTORY).then(setAllocationFactoryContracts);
    }
    if (proposalType === "set_enable_result_contracts") {
      fetchContractsByTemplate(TEMPLATE_REGISTRAR_SERVICE).then(setRegistrarServiceContracts);
    }
    // Provision Instrument needs the typed rows to filter on the registrar
    // field, so it uses /services/registrar instead of the blob query.
    if (proposalType === "provision_instrument") {
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

  useEffect(() => {
    if (selectedActionType === "dev_net_feature_app") {
      fetchNetworkInfo();
    }
  }, [selectedActionType, fetchNetworkInfo]);

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
                    sx: { fontFamily: "monospace", fontSize: "0.8rem" },
                    endAdornment: fieldHelpAdornment(opts.help, `Help for ${opts.label}`),
                  },
                }
          }
        >
          {many &&
            activeDelegations.map((d, idx) => (
              <MenuItem key={d.cid} value={d.cid}>
                <Typography variant="caption" sx={{ fontFamily: "monospace" }}>
                  {d.cid.slice(0, 24)}… — {describe(d)}
                  {idx === 0 ? " — in use" : ""}
                </Typography>
              </MenuItem>
            ))}
        </TextField>
      </>
    );
  };

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


  // Build ActionType from form state
  const buildActionFromForm = (): ActionType | null => {
    switch (selectedActionType) {
      case "governance_add_member":
        return {
          type: "governance_add_member",
          member: memberParty,
          new_threshold: newThreshold,
        };
      case "governance_remove_member":
        return {
          type: "governance_remove_member",
          member: memberParty,
          new_threshold: newThreshold,
        };
      case "governance_set_threshold":
        return {
          type: "governance_set_threshold",
          new_threshold: newThreshold,
        };
      case "governance_set_timeout":
        return {
          type: "governance_set_timeout",
          new_timeout_microseconds: timeoutMicroseconds,
        };
      case "utility_create_provider_request":
        return {
          type: "utility_create_provider_request",
          operator: operatorParty,
        };
      case "utility_create_user_request":
        return { type: "utility_create_user_request", operator: operatorParty };
      case "utility_setup":
        return {
          type: "utility_setup",
          operator: operatorParty,
          provider_service_cid: providerServiceCid,
          user_service_cid: userServiceCid,
        };
      case "utility_accept_holder_service_request":
        return {
          type: "utility_accept_holder_service_request",
          operator: operatorParty,
          provider_service_cid: providerServiceCid,
          holder_service_request_cid: holderServiceRequestCid,
          holder: holderParty,
        };
      case "credential_offer_free":
        return {
          type: "credential_offer_free",
          operator: operatorParty,
          user_service_cid: userServiceCid,
          holder: holderParty,
          id: credentialId,
          description: credentialDescription,
          claims,
        };
      case "credential_accept_free":
        return {
          type: "credential_accept_free",
          operator: operatorParty,
          user_service_cid: userServiceCid,
          credential_offer_cid: credentialOfferCid,
        };
      case "dev_net_feature_app":
        return {
          type: "dev_net_feature_app",
          amulet_rules_cid: amuletRulesCid,
        };
      default:
        return null;
    }
  };

  // Parse a multi-line "subject,property,value" textarea into a Claim[].
  // Mirrors the comma-split-with-error-on-bad-line pattern used by
  // set_provider_app_reward_beneficiaries.
  const parseClaimsText = (text: string): Claim[] => {
    const lines = text
      .split("\n")
      .map((line) => line.trim())
      .filter(Boolean);
    return lines.map((line, idx) => {
      const parts = line.split(",").map((s) => s.trim());
      if (parts.length !== 3 || !parts[0] || !parts[1] || !parts[2]) {
        throw new Error(
          `Claim line ${idx + 1}: expected "<subject>,<property>,<value>", got "${line}"`,
        );
      }
      return { subject: parts[0], property: parts[1], value: parts[2] };
    });
  };

  // Parse one requirement row's claims textarea. Each line is
  // "<property>,<value>" — parseClaimsText minus the subject, because a
  // requirement applies to whichever party gets credentialed.
  const parseRequiredClaimsText = (
    text: string,
  ): { property: string; value: string }[] => {
    const lines = text
      .split("\n")
      .map((line) => line.trim())
      .filter(Boolean);
    return lines.map((line, idx) => {
      const parts = line.split(",").map((s) => s.trim());
      if (parts.length !== 2 || !parts[0] || !parts[1]) {
        throw new Error(
          `Claim line ${idx + 1}: expected "<property>,<value>", got "${line}"`,
        );
      }
      return { property: parts[0], value: parts[1] };
    });
  };

  // Turn requirement-editor rows into PartyCredentialRequirement payloads.
  // `label` names the list in error messages.
  const buildRequirements = (
    rows: { issuer: string; claimsText: string }[],
    label: string,
  ): PartyCredentialRequirement[] =>
    rows.map((row, idx) => {
      const issuer = row.issuer.trim();
      if (!issuer) {
        throw new Error(`${label} requirement ${idx + 1}: issuer party is required`);
      }
      return {
        issuer,
        required_claims: parseRequiredClaimsText(row.claimsText),
      };
    });

  // Split a one-party-per-line textarea into party ids. A duplicated party
  // is an error, not silently deduplicated: the templates refuse duplicates
  // (two mints for one subject would share a credential id), and dropping a
  // line here would hide what the operator actually typed.
  const parsePartyLines = (text: string): string[] => {
    const lines = text
      .split("\n")
      .map((line) => line.trim())
      .filter(Boolean);
    const seen = new Set<string>();
    for (const line of lines) {
      if (seen.has(line)) {
        throw new Error(`Party id listed more than once: ${line}`);
      }
      seen.add(line);
    }
    return lines;
  };

  // Daml's Decimal scale for a reward-split percentage: 10 places.
  const SPLIT_SCALE = 10_000_000_000n;

  /** Render a scaled integer as the fixed-point decimal the ledger stores. */
  const formatSplitShare = (scaled: bigint): string => {
    const s = scaled.toString().padStart(11, "0");
    return `${s.slice(0, -10)}.${s.slice(-10)}`;
  };

  /**
   * Turn integer weights into shares that sum to EXACTLY 1.0.
   *
   * The ledger compares the sum as exact Decimal, with no tolerance, so an even
   * 3-way split is not expressible as a repeated decimal: 0.3333333333 three
   * times is 0.9999999999 and the vote fails at execute, after the
   * confirmations are already spent. Rather than ask a human to hand-balance
   * the last entry, take integer weights and derive the decimals.
   *
   * Floor each `weight / total`, then give every leftover unit to the LARGEST
   * weight (ties by row order). The rule is deterministic on purpose: a
   * confirmer has to be able to reproduce the split from the weights alone, so
   * "whichever row happened to be picked" is not good enough. Distortion is at
   * most (n-1) × 1e-10.
   *
   * Returns null when the weights cannot produce a valid split.
   */
  const splitFromWeights = (weights: bigint[]): bigint[] | null => {
    const total = weights.reduce((a, w) => a + w, 0n);
    if (total <= 0n) return null;
    const shares = weights.map((w) => (w * SPLIT_SCALE) / total);
    const leftover = SPLIT_SCALE - shares.reduce((a, s) => a + s, 0n);
    if (leftover > 0n) {
      let largest = 0;
      weights.forEach((w, i) => {
        if (w > weights[largest]) largest = i;
      });
      shares[largest] += leftover;
    }
    return shares;
  };

  /** Index of the row that absorbs the rounding remainder, for the UI to mark. */
  const splitRemainderRow = (weights: bigint[]): number => {
    let largest = 0;
    weights.forEach((w, i) => {
      if (w > weights[largest]) largest = i;
    });
    return largest;
  };

  /** Parse the weight column; null if any entry is not a positive integer. */
  const parseSplitWeights = (
    split: { beneficiary: string; weight: string }[],
  ): bigint[] | null => {
    const out: bigint[] = [];
    for (const row of split) {
      const w = row.weight.trim();
      if (!/^\d+$/.test(w)) return null;
      const v = BigInt(w);
      if (v <= 0n) return null;
      out.push(v);
    }
    return out;
  };

  const validateDelegationSplit = (
    split: { beneficiary: string; weight: string }[],
  ): string | null => {
    if (split.length === 0) return "Add at least one beneficiary";
    if (split.length > 20) return "At most 20 beneficiaries";
    if (split.some((b) => !b.beneficiary.trim())) {
      return "Every row needs a beneficiary party";
    }
    const parties = split.map((b) => b.beneficiary.trim());
    if (new Set(parties).size !== parties.length) {
      return "Each beneficiary may appear only once";
    }
    const weights = parseSplitWeights(split);
    if (!weights) return "Each weight must be a whole number greater than 0";
    const shares = splitFromWeights(weights);
    if (!shares) return "Weights must add up to more than 0";
    // Daml requires every percentage in (0, 1]. A weight tiny enough against the
    // total floors to zero, which the ledger rejects — catch it here instead.
    const zeroAt = shares.findIndex((s) => s <= 0n);
    if (zeroAt >= 0) {
      return `Row ${zeroAt + 1}'s weight is too small against the total — its share rounds to 0, which the ledger rejects`;
    }
    return null;
  };

  // Clear the action form fields after a successful submit so the next
  // action starts blank — keeps the form expanded and the submit button
  // visible (the new action shows up in the notification queue on its own).
  // NOTE: operatorParty / dsoPartyId / amuletRulesCid are intentionally NOT
  // cleared — they're autofetched (or seeded once) and should persist across
  // submissions of the same dialog session.
  const resetActionForm = () => {
    setMemberParty("");
    // Reset to latest applied governance values rather than hardcoded
    // 2 / 1h — the next form opening should reflect on-chain state. Clearing
    // the edit refs also lets the auto-seed effects fire again for any
    // values that haven't loaded yet at reset time.
    userEditedThresholdRef.current = false;
    userEditedTimeoutRef.current = false;
    setNewThreshold(
      governanceState?.threshold != null
        ? Number(governanceState.threshold)
        : 2,
    );
    setTimeoutMicroseconds(
      governanceState?.action_confirmation_timeout_microseconds ?? 3600000000,
    );
    setProviderServiceCid("");
    setUserServiceCid("");
    setHolderServiceRequestCid("");
    setHolderParty("");
    setCredentialId("");
    setCredentialDescription("");
    setCredentialOfferCid("");
    setClaims([]);
  };

  const handleSubmitAction = async () => {
    if (!rulesContractId) {
      setError("Please enter the Governance contract ID");
      return;
    }

    const action = buildActionFromForm();
    if (!action) {
      setError("Invalid action type");
      return;
    }

    setFormLoading(true);
    setError(null);

    try {
      const request: ConfirmActionRequest = {
        party_id: partyId,
        rules_contract_id: rulesContractId,
        action,
        governance_type: governanceType,
      };

      const res = await authenticatedFetch(`${API_BASE}/governance/confirm`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(request),
      });

      if (!res.ok) {
        const errData = await res.json().catch(() => ({}));
        throw new Error(errData.error || "Failed to submit confirmation");
      }

      // Clear fields, keep the form visible. The created action shows up
      // in the notification queue — no separate success message needed.
      resetActionForm();
      await Promise.all([fetchGovernance(), fetchGovernanceStateForPrefill()]);
      onAfterAction?.();
    } catch (e) {
      setError(
        e instanceof Error ? e.message : "Failed to submit confirmation",
      );
    } finally {
      setFormLoading(false);
    }
  };

  // Same idea as resetActionForm but for the proposal half. Mint/Burn re-seed
  // instrument_admin = partyId via a useEffect on proposalType change, but
  // because proposalType isn't changing here we re-seed it manually so it
  // stays populated after a successful submit.
  // NOTE: proposalOperator / proposalExpectedDso are intentionally NOT
  // cleared — they're autofetched (operator from /operator-info, DSO from
  // /network-info) and should persist across submissions.
  const resetProposalForm = () => {
    setProposalProvider("");
    setProposalInstrumentAdmin("");
    setProposalInstrumentAllowances([]);
    setProposalTransferFactoryCid("");
    setProposalExpectedAdmin("");
    setProposalReceiver("");
    setProposalAmount("");
    setSelectedHoldingKey("");
    setShowTransferAdvanced(false);
    setProposalInstrumentIdAdmin(
      proposalType === "mint" || proposalType === "burn" ? partyId : "",
    );
    setProposalInstrumentIdId("");
    setProposalInputHoldingCids("");
    setProposalTransferExpiryHours(String(DEFAULT_TRANSFER_EXPIRY_HOURS));
    setProposalTransferInstructionCid("");
    setProposalDescription("");
    setProposalProviderServiceCid("");
    setProposalInstrumentIdText("");
    setProposalCreateTransferRule(true);
    setProposalCreateAllocationFactory(true);
    setProposalUser("");
    setProposalInstrumentConfigurationCid("");
    setProposalBeneficiaries([]);
    setProposalClearBeneficiaries(false);
    setProposalDelegationDso("");
    setProposalDelegationAssigners([]);
    setProposalDelegationSplit([]);
    setProposalPriorDelegation("");
    setProposalRevokeDelegationCid("");
    setProposalRegistrarServiceCid("");
    setProposalEnableResultContracts("true");
    setProposalAllocationFactoryCid("");
    setProposalRecipient("");
    setProposalHolder("");
    setProposalUserServiceCid("");
    setProposalCredentialId("");
    setProposalCredentialClaimsText("");
    setProposalCredentialOfferCid("");
    setProposalIssuerCredentialCids([]);
    setProposalDelegate("");
    setProposalDelegationExpiresAt("");
    setProposalAmuletMergeLimit("10");
    setProposalRegistrarServiceRequestCid("");
    setProposalProviderConfigurationCid("");
    setProposalRegistrarRequirements([]);
    setProposalHolderRequirements([]);
    setProposalIssuerRequirements([]);
    setProposalInitialInstrumentIssuersText("");
    setProposalInstrumentIssuersText("");
    setProposalOffboardRows([]);
  };

  const handleSubmitProposal = async () => {
    if (!rulesContractId) return;
    setProposalLoading(true);
    setError(null);

    try {
      let proposal: ProposalType;
      switch (proposalType) {
        case "setup_cc_preapproval":
          proposal = {
            type: "setup_cc_preapproval",
            provider: proposalProvider,
            expected_dso: proposalExpectedDso,
          };
          break;
        case "setup_token_preapproval":
          proposal = {
            type: "setup_token_preapproval",
            operator: proposalOperator,
            instrument_admin: proposalInstrumentAdmin,
            // Strip the local-only `uid` field and drop empty rows.
            instrument_allowances: proposalInstrumentAllowances
              .filter((a) => a.id.trim() !== "")
              .map(({ id }) => ({ id })),
          };
          break;
        case "transfer": {
          // Send the override only when it's a valid positive integer that
          // differs from the default; otherwise omit it and let the backend
          // apply its default window.
          const expiryHours = Number(proposalTransferExpiryHours);
          const validityWindowHours =
            Number.isInteger(expiryHours) &&
            expiryHours > 0 &&
            expiryHours !== DEFAULT_TRANSFER_EXPIRY_HOURS
              ? expiryHours
              : undefined;
          proposal = {
            type: "transfer",
            transfer_factory_cid: proposalTransferFactoryCid,
            expected_admin: proposalExpectedAdmin,
            receiver: proposalReceiver,
            amount: proposalAmount,
            instrument_id: { admin: proposalInstrumentIdAdmin, id: proposalInstrumentIdId },
            input_holding_cids: proposalInputHoldingCids ? proposalInputHoldingCids.split(",").map((s) => s.trim()).filter(Boolean) : [],
            ...(validityWindowHours !== undefined && {
              validity_window_hours: validityWindowHours,
            }),
          };
          break;
        }
        case "accept_transfer":
          proposal = {
            type: "accept_transfer",
            transfer_instruction_cid: proposalTransferInstructionCid,
          };
          break;
        case "generic_vote":
          proposal = {
            type: "generic_vote",
            description: proposalDescription,
          };
          break;
        case "provision_provider_service":
          proposal = { type: "provision_provider_service" };
          break;
        case "setup_utility":
          proposal = {
            type: "setup_utility",
            provider_service_cid: proposalProviderServiceCid,
            operator: proposalOperator,
            instrument_id_text: proposalInstrumentIdText,
            additional_identifiers: [],
            create_transfer_rule: proposalCreateTransferRule,
            create_allocation_factory: proposalCreateAllocationFactory,
          };
          break;
        case "create_provider_service_request":
          proposal = {
            type: "create_provider_service_request",
            operator: proposalOperator,
            provider: proposalProvider,
          };
          break;
        case "create_user_service_request":
          proposal = {
            type: "create_user_service_request",
            operator: proposalOperator,
            user: proposalUser,
          };
          break;
        case "set_provider_app_reward_beneficiaries": {
          let beneficiaries: AppRewardBeneficiary[] | null = null;
          if (!proposalClearBeneficiaries) {
            beneficiaries = proposalBeneficiaries.map((b, idx) => {
              const party = b.beneficiary.trim();
              const weight = b.weight.trim();
              if (!party || !weight) {
                throw new Error(
                  `Beneficiary row ${idx + 1}: party and weight are required`,
                );
              }
              return { beneficiary: party, weight };
            });
          }
          proposal = {
            type: "set_provider_app_reward_beneficiaries",
            instrument_configuration_cid: proposalInstrumentConfigurationCid,
            provider_app_reward_beneficiaries: beneficiaries ?? undefined,
          };
          break;
        }
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
          proposal = {
            type: "set_enable_result_contracts",
            registrar_service_cid: proposalRegistrarServiceCid,
            enable_result_contracts:
              proposalEnableResultContracts === "clear"
                ? undefined
                : proposalEnableResultContracts === "true",
          };
          break;
        case "create_delegated_batched_markers_proxy":
          proposal = {
            type: "create_delegated_batched_markers_proxy",
            operator: proposalOperator,
          };
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
        case "mint":
          proposal = {
            type: "mint",
            allocation_factory_cid: proposalAllocationFactoryCid,
            instrument_id: { admin: proposalInstrumentIdAdmin, id: proposalInstrumentIdId },
            instrument_configuration_cid: proposalInstrumentConfigurationCid,
            recipient: proposalRecipient,
            amount: proposalAmount,
            description: proposalDescription,
          };
          break;
        case "burn":
          proposal = {
            type: "burn",
            allocation_factory_cid: proposalAllocationFactoryCid,
            instrument_id: { admin: proposalInstrumentIdAdmin, id: proposalInstrumentIdId },
            instrument_configuration_cid: proposalInstrumentConfigurationCid,
            holder: proposalHolder,
            amount: proposalAmount,
            description: proposalDescription,
          };
          break;
        case "accept_mint_request":
          proposal = {
            type: "accept_mint_request",
            mint_request_cid: proposalMintRequestCid,
            instrument_configuration_cid: proposalInstrumentConfigurationCid,
            issuer_credential_cids: proposalIssuerCredentialCids,
            description: proposalDescription,
          };
          break;
        case "accept_burn_request":
          proposal = {
            type: "accept_burn_request",
            burn_request_cid: proposalBurnRequestCid,
            instrument_configuration_cid: proposalInstrumentConfigurationCid,
            issuer_credential_cids: proposalIssuerCredentialCids,
            description: proposalDescription,
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
          throw new Error(
            "Paid credential proposal forms are not implemented yet — use the Free direction or call the API directly.",
          );
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
          };
          break;
        case "onboard_instrument_issuers":
          proposal = {
            type: "onboard_instrument_issuers",
            instrument_configuration_cid: proposalInstrumentConfigurationCid,
            instrument_issuers: parsePartyLines(proposalInstrumentIssuersText),
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

  // Render form fields based on selected action type
  const renderActionFormFields = () => {
    switch (selectedActionType) {
      case "governance_add_member":
        return (
          <>
            <TextField
              label="Member Party ID"
              value={memberParty}
              onChange={(e) => setMemberParty(e.target.value)}
              size="small"
              fullWidth
              sx={{ mb: 2 }}
              slotProps={{
                input: {
                  endAdornment: fieldHelpAdornment(
                    "Party id of the new governance member to add to this rules contract.",
                    "Help for Member Party ID",
                  ),
                },
              }}
            />
            <TextField
              label="New Threshold"
              type="number"
              value={newThreshold}
              onChange={(e) => {
                userEditedThresholdRef.current = true;
                setNewThreshold(parseInt(e.target.value) || 2);
              }}
              size="small"
              fullWidth
              slotProps={{
                input: {
                  endAdornment: fieldHelpAdornment(
                    "Number of governance member confirmations required to execute an action after this member is added.",
                    "Help for New Threshold",
                  ),
                },
              }}
            />
          </>
        );
      case "governance_remove_member": {
        // Source of truth for the current member list is the active rules
        // contract — `governanceState.members` is populated by the
        // /governance/state fetch. If it hasn't loaded yet (or for some
        // reason returns empty), fall back to the freeform text field so
        // the user isn't blocked.
        const members = governanceState?.members ?? [];
        return (
          <>
            {members.length > 0 ? (
              <TextField
                select
                label="Member to remove"
                value={memberParty}
                onChange={(e) => setMemberParty(e.target.value)}
                size="small"
                fullWidth
                sx={{ mb: 2 }}
                slotProps={{
                  input: {
                    endAdornment: fieldHelpAdornment(
                      "Pick which existing governance member to remove from this rules contract.",
                      "Help for Member to remove",
                    ),
                  },
                }}
              >
                {members.map((id) => (
                  <MenuItem key={id} value={id}>
                    {id}
                  </MenuItem>
                ))}
              </TextField>
            ) : (
              <TextField
                label="Member Party ID"
                value={memberParty}
                onChange={(e) => setMemberParty(e.target.value)}
                size="small"
                fullWidth
                sx={{ mb: 2 }}
                helperText="Members list not loaded — paste the party id directly"
                slotProps={{
                  input: {
                    endAdornment: fieldHelpAdornment(
                      "Party id of the governance member to remove.",
                      "Help for Member Party ID",
                    ),
                  },
                }}
              />
            )}
            <TextField
              label="New Threshold"
              type="number"
              value={newThreshold}
              onChange={(e) => {
                userEditedThresholdRef.current = true;
                setNewThreshold(parseInt(e.target.value) || 2);
              }}
              size="small"
              fullWidth
              slotProps={{
                input: {
                  endAdornment: fieldHelpAdornment(
                    "Number of governance member confirmations required to execute an action after this member is removed.",
                    "Help for New Threshold",
                  ),
                },
              }}
            />
          </>
        );
      }
      case "governance_set_threshold":
        return (
          <TextField
            label="New Threshold"
            type="number"
            value={newThreshold}
            onChange={(e) => {
              userEditedThresholdRef.current = true;
              setNewThreshold(parseInt(e.target.value) || 2);
            }}
            size="small"
            fullWidth
            slotProps={{
              input: {
                endAdornment: fieldHelpAdornment(
                  "Number of governance member confirmations required to execute an action.",
                  "Help for New Threshold",
                ),
              },
            }}
          />
        );
      case "governance_set_timeout":
        return (
          <TextField
            label="Timeout (microseconds)"
            type="number"
            value={timeoutMicroseconds}
            onChange={(e) => {
              userEditedTimeoutRef.current = true;
              setTimeoutMicroseconds(parseInt(e.target.value) || 0);
            }}
            size="small"
            fullWidth
            helperText="1 hour = 3,600,000,000 microseconds"
            slotProps={{
              input: {
                endAdornment: fieldHelpAdornment(
                  "How long a confirmation stays valid before expiring, in microseconds. 1 hour = 3,600,000,000.",
                  "Help for Timeout",
                ),
              },
            }}
          />
        );
      case "utility_create_provider_request":
      case "utility_create_user_request":
        return (
          <TextField
            label="Operator Party"
            value={operatorParty}
            onChange={(e) => setOperatorParty(e.target.value)}
            size="small"
            fullWidth
            slotProps={{
              input: {
                endAdornment: fieldHelpAdornment(
                  "Party id of the utility operator that will sign off on this onboarding request.",
                  "Help for Operator Party",
                ),
              },
            }}
          />
        );
      case "utility_setup":
        return (
          <>
            <TextField
              label="Operator Party"
              value={operatorParty}
              onChange={(e) => setOperatorParty(e.target.value)}
              size="small"
              fullWidth
              sx={{ mb: 2 }}
              slotProps={{
                input: {
                  endAdornment: fieldHelpAdornment(
                    "Party id of the utility operator setting up the registrar service.",
                    "Help for Operator Party",
                  ),
                },
              }}
            />
            <Box sx={{ display: "flex", gap: 1, alignItems: "center", mb: 2 }}>
              <FormControl fullWidth size="small">
                <InputLabel>
                  <TextHelp text="ProviderService contract this party already has from the operator.">
                    Provider Service
                  </TextHelp>
                </InputLabel>
                <Select
                  value={providerServiceCid}
                  label="Provider Service"
                  onChange={(e) => setProviderServiceCid(e.target.value)}
                  MenuProps={{ disableScrollLock: true }}
                >
                  {servicesLoading ? (
                    <MenuItem disabled>Loading services...</MenuItem>
                  ) : providerServices.length > 0 ? (
                    providerServices.map((svc) => (
                      <MenuItem key={svc.contract_id} value={svc.contract_id}>
                        {svc.contract_id}
                      </MenuItem>
                    ))
                  ) : (
                    <MenuItem disabled>No provider services found</MenuItem>
                  )}
                </Select>
              </FormControl>
              <Tooltip title="Refresh">
                <span>
                  <IconButton
                    size="small"
                    onClick={fetchServices}
                    disabled={servicesLoading}
                  >
                    {servicesLoading ? <CircularProgress size={20} /> : <RefreshIcon />}
                  </IconButton>
                </span>
              </Tooltip>
            </Box>
            <Box sx={{ display: "flex", gap: 1, alignItems: "center" }}>
              <FormControl fullWidth size="small">
                <InputLabel>
                  <TextHelp text="UserService contract this party already has from the operator.">
                    User Service
                  </TextHelp>
                </InputLabel>
                <Select
                  value={userServiceCid}
                  label="User Service"
                  onChange={(e) => setUserServiceCid(e.target.value)}
                  MenuProps={{ disableScrollLock: true }}
                >
                  {servicesLoading ? (
                    <MenuItem disabled>Loading services...</MenuItem>
                  ) : userServices.length > 0 ? (
                    userServices.map((svc) => (
                      <MenuItem key={svc.contract_id} value={svc.contract_id}>
                        {svc.contract_id}
                      </MenuItem>
                    ))
                  ) : (
                    <MenuItem disabled>No user services found</MenuItem>
                  )}
                </Select>
              </FormControl>
              <Tooltip title="Refresh">
                <span>
                  <IconButton
                    size="small"
                    onClick={fetchServices}
                    disabled={servicesLoading}
                  >
                    {servicesLoading ? <CircularProgress size={20} /> : <RefreshIcon />}
                  </IconButton>
                </span>
              </Tooltip>
            </Box>
          </>
        );
      case "utility_accept_holder_service_request":
        return (
          <>
            <TextField
              label="Operator Party"
              value={operatorParty}
              onChange={(e) => setOperatorParty(e.target.value)}
              size="small"
              fullWidth
              sx={{ mb: 2 }}
              slotProps={{
                input: {
                  endAdornment: fieldHelpAdornment(
                    "Party id of the utility operator that issued the ProviderService.",
                    "Help for Operator Party",
                  ),
                },
              }}
            />
            <Box sx={{ display: "flex", gap: 1, alignItems: "center", mb: 2 }}>
              <FormControl fullWidth size="small">
                <InputLabel>
                  <TextHelp text="ProviderService contract this party will exercise to accept the holder's request.">
                    Provider Service
                  </TextHelp>
                </InputLabel>
                <Select
                  value={providerServiceCid}
                  label="Provider Service"
                  onChange={(e) => setProviderServiceCid(e.target.value)}
                  MenuProps={{ disableScrollLock: true }}
                >
                  {servicesLoading ? (
                    <MenuItem disabled>Loading services...</MenuItem>
                  ) : providerServices.length > 0 ? (
                    providerServices.map((svc) => (
                      <MenuItem key={svc.contract_id} value={svc.contract_id}>
                        {svc.contract_id}
                      </MenuItem>
                    ))
                  ) : (
                    <MenuItem disabled>No provider services found</MenuItem>
                  )}
                </Select>
              </FormControl>
              <Tooltip title="Refresh">
                <span>
                  <IconButton
                    size="small"
                    onClick={fetchServices}
                    disabled={servicesLoading}
                  >
                    {servicesLoading ? <CircularProgress size={20} /> : <RefreshIcon />}
                  </IconButton>
                </span>
              </Tooltip>
            </Box>
            <TextField
              label="Holder Service Request Contract ID"
              value={holderServiceRequestCid}
              onChange={(e) => setHolderServiceRequestCid(e.target.value)}
              size="small"
              fullWidth
              sx={{ mb: 2 }}
              slotProps={{
                input: {
                  endAdornment: fieldHelpAdornment(
                    "Contract id of the pending HolderServiceRequest that this proposal will accept.",
                    "Help for Holder Service Request Contract ID",
                  ),
                },
              }}
            />
            <TextField
              label="Holder Party"
              value={holderParty}
              onChange={(e) => setHolderParty(e.target.value)}
              size="small"
              fullWidth
              slotProps={{
                input: {
                  endAdornment: fieldHelpAdornment(
                    "Party id of the holder whose service request is being accepted.",
                    "Help for Holder Party",
                  ),
                },
              }}
            />
          </>
        );
      case "credential_offer_free":
        return (
          <>
            <TextField
              label="Operator Party"
              value={operatorParty}
              onChange={(e) => setOperatorParty(e.target.value)}
              size="small"
              fullWidth
              sx={{ mb: 2 }}
              slotProps={{
                input: {
                  endAdornment: fieldHelpAdornment(
                    "Party id of the utility operator the user service is registered with.",
                    "Help for Operator Party",
                  ),
                },
              }}
            />
            <Box sx={{ display: "flex", gap: 1, alignItems: "center", mb: 2 }}>
              <FormControl fullWidth size="small">
                <InputLabel>
                  <TextHelp text="UserService contract that will issue the credential offer.">
                    User Service
                  </TextHelp>
                </InputLabel>
                <Select
                  value={userServiceCid}
                  label="User Service"
                  onChange={(e) => setUserServiceCid(e.target.value)}
                  MenuProps={{ disableScrollLock: true }}
                >
                  {servicesLoading ? (
                    <MenuItem disabled>Loading services...</MenuItem>
                  ) : userServices.length > 0 ? (
                    userServices.map((svc) => (
                      <MenuItem key={svc.contract_id} value={svc.contract_id}>
                        {svc.contract_id}
                      </MenuItem>
                    ))
                  ) : (
                    <MenuItem disabled>No user services found</MenuItem>
                  )}
                </Select>
              </FormControl>
              <Tooltip title="Refresh">
                <span>
                  <IconButton
                    size="small"
                    onClick={fetchServices}
                    disabled={servicesLoading}
                  >
                    {servicesLoading ? <CircularProgress size={20} /> : <RefreshIcon />}
                  </IconButton>
                </span>
              </Tooltip>
            </Box>
            <TextField
              label="Holder Party"
              value={holderParty}
              onChange={(e) => setHolderParty(e.target.value)}
              size="small"
              fullWidth
              sx={{ mb: 2 }}
              slotProps={{
                input: {
                  endAdornment: fieldHelpAdornment(
                    "Party id that will receive the credential offer.",
                    "Help for Holder Party",
                  ),
                },
              }}
            />
            <TextField
              label="Credential ID"
              value={credentialId}
              onChange={(e) => setCredentialId(e.target.value)}
              size="small"
              fullWidth
              sx={{ mb: 2 }}
              slotProps={{
                input: {
                  endAdornment: fieldHelpAdornment(
                    "Unique identifier for this credential (free-form string).",
                    "Help for Credential ID",
                  ),
                },
              }}
            />
            <TextField
              label="Credential Description"
              value={credentialDescription}
              onChange={(e) => setCredentialDescription(e.target.value)}
              size="small"
              fullWidth
              sx={{ mb: 2 }}
              slotProps={{
                input: {
                  endAdornment: fieldHelpAdornment(
                    "Free-form human-readable description of what this credential certifies.",
                    "Help for Credential Description",
                  ),
                },
              }}
            />
            <Typography variant="caption" color="text.secondary">
              <TextHelp text="Statements baked into the credential. Each row is a (subject, property, value) triple.">
                Claims
              </TextHelp>
            </Typography>
            {claims.map((claim, idx) => (
              <Box key={idx} sx={{ display: "flex", gap: 1, mb: 1 }}>
                <TextField
                  label="Subject"
                  value={claim.subject}
                  onChange={(e) => {
                    const updated = [...claims];
                    updated[idx] = { ...claim, subject: e.target.value };
                    setClaims(updated);
                  }}
                  size="small"
                  sx={{ flex: 1 }}
                  slotProps={{
                    input: {
                      endAdornment: fieldHelpAdornment(
                        "What this claim is about (e.g. the holder party id).",
                        "Help for Subject",
                      ),
                    },
                  }}
                />
                <TextField
                  label="Property"
                  value={claim.property}
                  onChange={(e) => {
                    const updated = [...claims];
                    updated[idx] = { ...claim, property: e.target.value };
                    setClaims(updated);
                  }}
                  size="small"
                  sx={{ flex: 1 }}
                  slotProps={{
                    input: {
                      endAdornment: fieldHelpAdornment(
                        "Attribute name being asserted (e.g. \"kyc_verified\").",
                        "Help for Property",
                      ),
                    },
                  }}
                />
                <TextField
                  label="Value"
                  value={claim.value}
                  onChange={(e) => {
                    const updated = [...claims];
                    updated[idx] = { ...claim, value: e.target.value };
                    setClaims(updated);
                  }}
                  size="small"
                  sx={{ flex: 1 }}
                  slotProps={{
                    input: {
                      endAdornment: fieldHelpAdornment(
                        "Value of the property (e.g. \"true\" or a region code).",
                        "Help for Value",
                      ),
                    },
                  }}
                />
                <Button
                  size="small"
                  color="error"
                  onClick={() => setClaims(claims.filter((_, i) => i !== idx))}
                >
                  Remove
                </Button>
              </Box>
            ))}
            <Button
              size="small"
              onClick={() =>
                setClaims([...claims, { subject: "", property: "", value: "" }])
              }
            >
              Add Claim
            </Button>
          </>
        );
      case "credential_accept_free":
        return (
          <>
            <TextField
              label="Operator Party"
              value={operatorParty}
              onChange={(e) => setOperatorParty(e.target.value)}
              size="small"
              fullWidth
              sx={{ mb: 2 }}
              slotProps={{
                input: {
                  endAdornment: fieldHelpAdornment(
                    "Party id of the utility operator the user service is registered with.",
                    "Help for Operator Party",
                  ),
                },
              }}
            />
            <Box sx={{ display: "flex", gap: 1, alignItems: "center", mb: 2 }}>
              <FormControl fullWidth size="small">
                <InputLabel>
                  <TextHelp text="UserService contract that will accept the credential offer on this party's behalf.">
                    User Service
                  </TextHelp>
                </InputLabel>
                <Select
                  value={userServiceCid}
                  label="User Service"
                  onChange={(e) => setUserServiceCid(e.target.value)}
                  MenuProps={{ disableScrollLock: true }}
                >
                  {servicesLoading ? (
                    <MenuItem disabled>Loading services...</MenuItem>
                  ) : userServices.length > 0 ? (
                    userServices.map((svc) => (
                      <MenuItem key={svc.contract_id} value={svc.contract_id}>
                        {svc.contract_id}
                      </MenuItem>
                    ))
                  ) : (
                    <MenuItem disabled>No user services found</MenuItem>
                  )}
                </Select>
              </FormControl>
              <Tooltip title="Refresh">
                <span>
                  <IconButton
                    size="small"
                    onClick={() => {
                      fetchServices();
                      fetchCredentialOffers();
                    }}
                    disabled={servicesLoading || credentialOffersLoading}
                  >
                    {servicesLoading ? <CircularProgress size={20} /> : <RefreshIcon />}
                  </IconButton>
                </span>
              </Tooltip>
            </Box>
            {renderCredentialOfferAutocomplete(
              credentialOfferCid,
              setCredentialOfferCid,
            )}
          </>
        );
      case "dev_net_feature_app":
        return (
          <Box sx={{ display: "flex", gap: 1, alignItems: "center" }}>
            <TextField
              label="Amulet Rules CID"
              value={amuletRulesCid}
              onChange={(e) => setAmuletRulesCid(e.target.value)}
              fullWidth
              size="small"
              required
              slotProps={{
                input: {
                  endAdornment: fieldHelpAdornment(
                    "Contract id of the active AmuletRules contract on devnet; needed to request a Featured App Right.",
                    "Help for Amulet Rules CID",
                  ),
                },
              }}
            />
            <Tooltip title="Refresh">
              <span>
                <IconButton
                  size="small"
                  onClick={fetchNetworkInfo}
                  disabled={amuletRulesLoading}
                >
                  {amuletRulesLoading ? <CircularProgress size={20} /> : <RefreshIcon />}
                </IconButton>
              </span>
            </Tooltip>
          </Box>
        );
      default:
        return null;
    }
  };

  if (loading && !data) {
    return (
      <Box sx={{ display: "flex", justifyContent: "center", p: 2 }}>
        <CircularProgress size={24} />
      </Box>
    );
  }

  return (
    <Box sx={{ mt: 2 }}>
      {/* Shared across both halves: the proposals view (`view="proposals"`)
          does not render `showActionsHalf`, so keeping the error here ensures
          a failed `/governance/propose` (or action) surfaces in either view
          instead of failing silently. */}
      {error && (
        <Alert severity="error" sx={{ mb: 2 }} onClose={() => setError(null)}>
          {error}
        </Alert>
      )}

      {showActionsHalf && (
      <>
      {view !== "actions" && (
        <Box
          sx={{
            display: "flex",
            alignItems: "center",
            cursor: "pointer",
            mb: 1,
          }}
          onClick={() => setExpanded(!expanded)}
        >
          <IconButton size="small">
            {expanded ? <ExpandLessIcon /> : <ExpandMoreIcon />}
          </IconButton>
          <Typography variant="subtitle2">
            Governance Actions
            {data && data.actions.length > 0 && (
              <Chip
                label={data.actions.length}
                size="small"
                sx={{ ml: 1 }}
                color="primary"
              />
            )}
          </Typography>
        </Box>
      )}

      <Collapse in={expanded}>
        {(data?.gov_core_out_of_date || governanceState?.out_of_date) && (
          <Alert severity="warning" sx={{ mb: 2 }}>
            The governance core contract is out of date
            {data?.gov_core_package_ref || governanceState?.package_ref
              ? ` (running on ${data?.gov_core_package_ref || governanceState?.package_ref})`
              : ""}
            . Actions are executed against the old package — the party should
            be migrated to the latest governance-core package.
          </Alert>
        )}

        {view !== "actions" && (
        <Box sx={{ mb: 2 }}>
          <Autocomplete
            freeSolo
            options={governanceContractIds}
            value={rulesContractId}
            onChange={(_e, value) => setRulesContractId(value || "")}
            onInputChange={(_e, value) => setRulesContractId(value)}
            disabled={!ADMIN_ACCESS}
            size="small"
            renderInput={(params) => (
              <TextField
                {...params}
                label={
                  <TextHelp text="Contract id of the GovernanceRules contract that all new actions will target. Defaults to this party's active rules.">
                    Governance Contract ID
                  </TextHelp>
                }
                placeholder="Enter or select contract ID"
              />
            )}
          />
        </Box>
        )}

        {/* New Action Form */}
        <Box sx={{ mb: 2 }}>
          {view !== "actions" && (
            <Button
              size="small"
              variant="outlined"
              startIcon={showNewActionForm ? <ExpandLessIcon /> : <AddIcon />}
              onClick={() => setShowNewActionForm(!showNewActionForm)}
              disabled={!ADMIN_ACCESS || !rulesContractId}
            >
              {showNewActionForm ? "Hide Form" : "New Governance Action"}
            </Button>
          )}

          <Collapse in={showNewActionForm}>
            <Box
              sx={
                view === "actions"
                  ? {}
                  : {
                      mt: 2,
                      p: 2,
                      border: "1px solid",
                      borderColor: "divider",
                      borderRadius: 1,
                    }
              }
            >
              {view !== "actions" && (
                <Typography variant="subtitle2" sx={{ mb: 2 }}>
                  Create New Governance Action
                </Typography>
              )}

              <FormControl fullWidth size="small" sx={{ mb: 2 }}>
                <InputLabel>
                  <TextHelp text="What kind of governance action to create. The fields below adapt to the selected type.">
                    Action Type
                  </TextHelp>
                </InputLabel>
                <Select
                  value={selectedActionType}
                  label="Action Type"
                  onChange={(e) =>
                    setSelectedActionType(e.target.value as ActionTypeKey)
                  }
                  MenuProps={{ disableScrollLock: true }}
                >
                  {getActionTypeOptions(network).filter((opt) => {
                    if (opt.hidden && governanceType !== "core_self") return false;
                    if (governanceType === "core_self") {
                      // For governance-core, only show self-management actions
                      const selfActions = ["governance_add_member", "governance_remove_member", "governance_set_threshold", "governance_set_timeout"];
                      return selfActions.includes(opt.value);
                    }
                    return !opt.hidden;
                  }).map(
                    (opt) => (
                      <MenuItem key={opt.value} value={opt.value}>
                        {opt.label}
                      </MenuItem>
                    ),
                  )}
                </Select>
              </FormControl>

              <Divider sx={{ my: 2 }} />

              {renderActionFormFields()}

              {(() => {
                const inlineSubmitBtn = (
                  <Button
                    variant="contained"
                    onClick={handleSubmitAction}
                    disabled={formLoading || !rulesContractId}
                    startIcon={
                      formLoading ? (
                        <CircularProgress size={16} />
                      ) : (
                        <CheckCircleIcon />
                      )
                    }
                  >
                    Submit Confirmation
                  </Button>
                );
                const portalSubmitBtn = (
                  <Button
                    onClick={handleSubmitAction}
                    disabled={formLoading || !rulesContractId}
                    startIcon={
                      formLoading ? <CircularProgress size={16} /> : undefined
                    }
                  >
                    Submit Confirmation
                  </Button>
                );
                return submitPortalEl ? (
                  <Portal container={submitPortalEl}>{portalSubmitBtn}</Portal>
                ) : (
                  <Box sx={{ mt: 2, display: "flex", gap: 1 }}>
                    {inlineSubmitBtn}
                    {view !== "actions" && (
                      <Button
                        variant="outlined"
                        onClick={() => setShowNewActionForm(false)}
                      >
                        Cancel
                      </Button>
                    )}
                  </Box>
                );
              })()}
            </Box>
          </Collapse>
        </Box>

      </Collapse>
      </>
      )}

      {/* Proposals — only for governance-core */}
      {showProposalsHalf && governanceType === "core_self" && data && (
        <Box sx={view === "proposals" ? {} : { mt: 2, mx: -2 }}>
          {view !== "proposals" && (
            <Box sx={{ display: "flex", justifyContent: "space-between", alignItems: "center", mb: 1, px: 2 }}>
              <Typography variant="subtitle2">
                Proposals
                {(data.domain_actions?.length ?? 0) > 0 && (
                  <Chip label={data.domain_actions!.length} size="small" sx={{ ml: 1 }} color="secondary" />
                )}
              </Typography>
              <Button
                size="small"
                variant="outlined"
                onClick={() => {
                  if (!showProposalForm && !dsoPartyId) fetchNetworkInfo();
                  setShowProposalForm(!showProposalForm);
                }}
              >
                {showProposalForm ? "Cancel" : "New Proposal"}
              </Button>
            </Box>
          )}

          <Collapse in={showProposalForm}>
            <Box
              sx={
                view === "proposals"
                  ? { display: "flex", flexDirection: "column", gap: 1.5 }
                  : { display: "flex", flexDirection: "column", gap: 1.5, mb: 2, p: 2, mx: 2, border: 1, borderColor: "divider", borderRadius: 2 }
              }
            >
              <FormControl size="small" fullWidth>
                <InputLabel>
                  <TextHelp text="What kind of proposal to create. The form fields below adapt to the selected type.">
                    Proposal Type
                  </TextHelp>
                </InputLabel>
                <Select
                  value={proposalType}
                  label="Proposal Type"
                  onChange={(e) => setProposalType(e.target.value as ProposalType["type"])}
                >
                  <ListSubheader sx={{ color: "primary.main", fontWeight: 600 }}>Governance Core</ListSubheader>
                  <MenuItem value="generic_vote">Generic Vote</MenuItem>
                  <Divider />
                  <ListSubheader sx={{ color: "primary.main", fontWeight: 600 }}>Token Custody</ListSubheader>
                  <MenuItem value="setup_cc_preapproval">Setup CC Preapproval</MenuItem>
                  <MenuItem value="setup_token_preapproval">Setup Token Preapproval</MenuItem>
                  <MenuItem value="transfer">Transfer</MenuItem>
                  <MenuItem value="accept_transfer">Accept Transfer</MenuItem>
                  <Divider />
                  <ListSubheader sx={{ color: "primary.main", fontWeight: 600 }}>Utility Onboarding</ListSubheader>
                  <ListSubheader sx={{ fontStyle: "italic", lineHeight: 1.5, pl: 4 }}>Onboarding (in order)</ListSubheader>
                  <MenuItem value="create_user_service_request">1. Create User Service Request</MenuItem>
                  <MenuItem value="create_provider_service_request">2. Create Provider Service Request</MenuItem>
                  <MenuItem value="setup_utility">3. Setup Utility</MenuItem>
                  <ListSubheader sx={{ fontStyle: "italic", lineHeight: 1.5, pl: 4 }}>Settings / Configuration</ListSubheader>
                  <MenuItem value="set_provider_app_reward_beneficiaries">Set Provider App Reward Beneficiaries</MenuItem>
                  <MenuItem value="set_enable_result_contracts">Set Enable Result Contracts</MenuItem>
                  <MenuItem value="create_delegated_batched_markers_proxy">Create Delegated Batched Markers Proxy</MenuItem>
                  {/*
                  Hidden per Notion "Clean up Utility Plugin" task — keep the
                  ProposalType variant + form field + submit handler wired so
                  existing API consumers still work; just not surfaced in the
                  dropdown for now.
                  <MenuItem value="provision_provider_service">Provision Provider Service</MenuItem>
                  */}
                  <ListSubheader sx={{ fontStyle: "italic", lineHeight: 1.5, pl: 4 }}>Actions</ListSubheader>
                  <MenuItem value="accept_mint_request">Accept Mint Request</MenuItem>
                  <MenuItem value="accept_burn_request">Accept Burn Request</MenuItem>
                  <Divider />
                  <ListSubheader sx={{ color: "primary.main", fontWeight: 600 }}>Dual Governance Utility Onboarding</ListSubheader>
                  <ListSubheader sx={{ fontStyle: "italic", lineHeight: 1.5, pl: 4 }}>Onboarding (in order)</ListSubheader>
                  <MenuItem value="create_user_service_request">1. Create User Service Request (as Provider)</MenuItem>
                  <MenuItem value="create_provider_service_request">2. Create Provider Service Request (as Provider)</MenuItem>
                  <MenuItem value="create_provider_configuration">3. Create Provider Configuration (as Provider)</MenuItem>
                  <MenuItem value="create_registrar_service_request">4. Create Registrar Service Request (as Registrar)</MenuItem>
                  <MenuItem value="onboard_registrar">5. Onboard Registrar (as Provider)</MenuItem>
                  <MenuItem value="provision_instrument">6. Provision Instrument (as Registrar)</MenuItem>
                  <ListSubheader sx={{ fontStyle: "italic", lineHeight: 1.5, pl: 4 }}>Instrument Management</ListSubheader>
                  <MenuItem value="onboard_instrument_issuers">Onboard Instrument Issuers (as Registrar)</MenuItem>
                  <MenuItem value="offboard_instrument_issuers">Offboard Instrument Issuers (as Registrar)</MenuItem>
                  <Divider />
                  <ListSubheader sx={{ color: "primary.main", fontWeight: 600 }}>Rewards</ListSubheader>
                  <MenuItem value="setup_minting_delegation">Setup Minting Delegation</MenuItem>
                  <MenuItem value="accept_external_party_setup">Accept External Party Setup</MenuItem>
                  <MenuItem value="setup_coupon_reassignment_delegation">Setup Coupon Reassignment Delegation</MenuItem>
                  <MenuItem value="revoke_coupon_reassignment_delegation">Revoke Coupon Reassignment Delegation</MenuItem>
                  <Divider />
                  <ListSubheader sx={{ color: "primary.main", fontWeight: 600 }}>Utility Credential</ListSubheader>
                  <MenuItem value="offer_free_credential">Offer Free Credential</MenuItem>
                  <MenuItem value="accept_free_credential">Accept Free Credential</MenuItem>
                  <MenuItem value="offer_paid_credential" disabled>
                    Offer Paid Credential (form coming soon)
                  </MenuItem>
                </Select>
              </FormControl>

              <Divider />

              {proposalType === "create_provider_service_request" &&
                (network === "testnet" || network === "mainnet") && (
                  <Alert severity="info">
                    On TestNet and MainNet, a credential from Digital Asset (DA)
                    is required before a Provider Service Request can be
                    accepted.
                  </Alert>
                )}

              {proposalType === "generic_vote" && (
                <TextField
                  size="small"
                  label="Vote Description"
                  value={proposalDescription}
                  onChange={(e) => setProposalDescription(e.target.value)}
                  fullWidth
                  required
                  multiline
                  minRows={2}
                  maxRows={6}
                  helperText="Describe what the governance members are voting on"
                  slotProps={{
                    input: {
                      endAdornment: fieldHelpAdornment(
                        "Free-form human-readable note describing what governance members are voting on.",
                        "Help for Vote Description",
                      ),
                    },
                  }}
                />
              )}

              {proposalType === "setup_cc_preapproval" && (
                <>
                  {preapprovalCounts.cc > 0 && (
                    <Alert severity="warning">
                      This party already has a Canton Coin TransferPreapproval;
                      issuing another would create a duplicate and burn fees again.
                    </Alert>
                  )}
                  <TextField
                    size="small"
                    label="Provider Party"
                    value={proposalProvider}
                    onChange={(e) => setProposalProvider(e.target.value)}
                    fullWidth
                    required
                    slotProps={{
                      input: {
                        endAdornment: fieldHelpAdornment(
                          "Party id of the app provider that will receive the TransferPreapproval (usually the Splice app provider).",
                          "Help for Provider Party",
                        ),
                      },
                    }}
                  />
                  <TextField
                    size="small"
                    label="Expected DSO Party"
                    value={proposalExpectedDso}
                    onChange={(e) => setProposalExpectedDso(e.target.value)}
                    fullWidth
                    required
                    slotProps={{
                      input: {
                        endAdornment: fieldHelpAdornment(
                          "Party id of the Splice DSO; the proposal verifies the AmuletRules contract belongs to this DSO.",
                          "Help for Expected DSO Party",
                        ),
                      },
                    }}
                  />
                </>
              )}

              {proposalType === "setup_token_preapproval" && (
                <>
                  {preapprovalCounts.token > 0 && (
                    <Alert severity="warning">
                      This party already has {preapprovalCounts.token} token
                      TransferPreapproval{preapprovalCounts.token === 1 ? "" : "s"};
                      issuing another for the same instrument would likely be redundant.
                    </Alert>
                  )}
                  <TextField
                    size="small"
                    label="Operator Party"
                    value={proposalOperator}
                    onChange={(e) => setProposalOperator(e.target.value)}
                    fullWidth
                    required
                    slotProps={{
                      input: {
                        endAdornment: fieldHelpAdornment(
                          "Party id of the utility operator that runs the token registrar.",
                          "Help for Operator Party",
                        ),
                      },
                    }}
                  />
                  <TextField
                    size="small"
                    label="Instrument Admin"
                    value={proposalInstrumentAdmin}
                    onChange={(e) => setProposalInstrumentAdmin(e.target.value)}
                    fullWidth
                    required
                    slotProps={{
                      input: {
                        endAdornment: fieldHelpAdornment(
                          "Issuer party of the token whose TransferPreapproval is being set up.",
                          "Help for Instrument Admin",
                        ),
                      },
                    }}
                  />
                  <Typography variant="caption" sx={{ display: "block" }} color="text.secondary">
                    <TextHelp text="Optional per-instrument allowance ids that limit which tokens this preapproval covers. Leave empty to cover all.">
                      Instrument Allowances (optional)
                    </TextHelp>
                  </Typography>
                  {proposalInstrumentAllowances.map((a) => (
                    <Box key={a.uid} sx={{ display: "flex", gap: 1, mb: 1 }}>
                      <TextField
                        label="Allowance ID"
                        value={a.id}
                        onChange={(e) =>
                          setProposalInstrumentAllowances((prev) =>
                            prev.map((row) =>
                              row.uid === a.uid
                                ? { ...row, id: e.target.value }
                                : row,
                            ),
                          )
                        }
                        size="small"
                        sx={{ flex: 1 }}
                        slotProps={{
                          input: {
                            endAdornment: fieldHelpAdornment(
                              "Identifier of an allowed instrument under this preapproval.",
                              "Help for Allowance ID",
                            ),
                          },
                        }}
                      />
                      <Button
                        size="small"
                        color="error"
                        onClick={() =>
                          setProposalInstrumentAllowances((prev) =>
                            prev.filter((row) => row.uid !== a.uid),
                          )
                        }
                      >
                        Remove
                      </Button>
                    </Box>
                  ))}
                  <Button
                    size="small"
                    onClick={() =>
                      setProposalInstrumentAllowances((prev) => [
                        ...prev,
                        { uid: crypto.randomUUID(), id: "" },
                      ])
                    }
                  >
                    Add Allowance
                  </Button>
                </>
              )}

              {proposalType === "transfer" && (
                <>
                  <TextField
                    select
                    size="small"
                    label="Instrument"
                    value={selectedHoldingKey}
                    onChange={(e) => setSelectedHoldingKey(e.target.value)}
                    fullWidth
                    required
                    disabled={transferPrefillLoading}
                    helperText={
                      transferPrefillLoading
                        ? "Loading holdings…"
                        : transferHoldings.length === 0
                          ? "No holdings available for this party"
                          : "Pick an instrument — admin, ID, factory CID and expected admin will be prefilled"
                    }
                    slotProps={{
                      input: {
                        endAdornment: fieldHelpAdornment(
                          "Token to transfer, picked from this party's holdings. Selecting one prefills the matching TransferFactory and expected admin.",
                          "Help for Instrument",
                        ),
                      },
                    }}
                  >
                    {transferHoldings.map((h) => {
                      const key = `${h.instrument_admin}::${h.instrument_id}`;
                      const hasFactory = transferFactories.some(
                        (f) => f.expected_admin === h.instrument_admin,
                      );
                      // Canton Coin's token-standard instrument_id is the
                      // literal "Amulet" — display it as "CC" to match the
                      // Holdings section.
                      const label =
                        h.instrument_id === "Amulet" ? "CC" : h.instrument_id;
                      return (
                        <MenuItem
                          key={key}
                          value={key}
                          disabled={!hasFactory}
                        >
                          {label} — available {holdingAvailable(h)}
                          {Number(h.locked_amount) > 0 &&
                            ` (${h.locked_amount} locked)`}
                          {!hasFactory && " (no factory available)"}
                        </MenuItem>
                      );
                    })}
                  </TextField>
                  {selectedHoldingKey &&
                    (() => {
                      const holding = transferHoldings.find(
                        (h) =>
                          `${h.instrument_admin}::${h.instrument_id}` ===
                          selectedHoldingKey,
                      );
                      return holding ? (
                        <Box
                          sx={{
                            display: "flex",
                            gap: 1,
                            flexWrap: "wrap",
                            alignItems: "center",
                          }}
                        >
                          <Chip
                            size="small"
                            label={`Available balance: ${holdingAvailable(holding)}`}
                            color="primary"
                          />
                          {Number(holding.locked_amount) > 0 && (
                            <Chip
                              size="small"
                              label={`Locked: ${holding.locked_amount}`}
                              variant="outlined"
                              color="warning"
                            />
                          )}
                          <Chip
                            size="small"
                            label={`Admin: ${holding.instrument_admin}`}
                            variant="outlined"
                          />
                        </Box>
                      ) : null;
                    })()}
                  <TextField
                    size="small"
                    label="Receiver Party"
                    value={proposalReceiver}
                    onChange={(e) => setProposalReceiver(e.target.value)}
                    fullWidth
                    required
                    slotProps={{
                      input: {
                        endAdornment: fieldHelpAdornment(
                          "Party id that will receive the transferred tokens.",
                          "Help for Receiver Party",
                        ),
                      },
                    }}
                  />
                  <TextField
                    size="small"
                    label="Amount"
                    value={proposalAmount}
                    onChange={(e) => setProposalAmount(e.target.value)}
                    fullWidth
                    required
                    type="number"
                    slotProps={{
                      htmlInput: { min: 0, step: "any" },
                      input: {
                        endAdornment: fieldHelpAdornment(
                          "How much of the selected instrument to transfer. Must be positive and not exceed the available balance.",
                          "Help for Amount",
                        ),
                      },
                    }}
                    error={(() => {
                      if (!proposalAmount) return false;
                      const n = Number(proposalAmount);
                      if (!Number.isFinite(n) || n <= 0) return true;
                      const holding = transferHoldings.find(
                        (h) =>
                          `${h.instrument_admin}::${h.instrument_id}` ===
                          selectedHoldingKey,
                      );
                      return holding ? n > holdingAvailable(holding) : false;
                    })()}
                    helperText={(() => {
                      if (!proposalAmount) return "";
                      const n = Number(proposalAmount);
                      if (!Number.isFinite(n) || n <= 0)
                        return "Enter a positive amount";
                      const holding = transferHoldings.find(
                        (h) =>
                          `${h.instrument_admin}::${h.instrument_id}` ===
                          selectedHoldingKey,
                      );
                      if (holding && n > holdingAvailable(holding)) {
                        return `Exceeds available balance (${holdingAvailable(holding)})`;
                      }
                      return "";
                    })()}
                  />
                  <TextField
                    size="small"
                    label="Offer expiry (hours)"
                    value={proposalTransferExpiryHours}
                    onChange={(e) =>
                      setProposalTransferExpiryHours(e.target.value)
                    }
                    fullWidth
                    type="number"
                    slotProps={{
                      htmlInput: { min: 1, step: 1 },
                      input: {
                        endAdornment: fieldHelpAdornment(
                          `How long the transfer stays valid (default ${DEFAULT_TRANSFER_EXPIRY_HOURS}h). If the recipient isn't preapproved, this becomes an offer they must accept before it expires; after expiry the escrowed funds are released back to you.`,
                          "Help for Offer expiry",
                        ),
                      },
                    }}
                    error={(() => {
                      const n = Number(proposalTransferExpiryHours);
                      return (
                        proposalTransferExpiryHours !== "" &&
                        (!Number.isInteger(n) || n <= 0)
                      );
                    })()}
                    helperText={(() => {
                      const n = Number(proposalTransferExpiryHours);
                      if (
                        proposalTransferExpiryHours !== "" &&
                        (!Number.isInteger(n) || n <= 0)
                      ) {
                        return "Enter a positive whole number of hours";
                      }
                      return "";
                    })()}
                  />
                  <Button
                    size="small"
                    variant="text"
                    onClick={() => setShowTransferAdvanced((v) => !v)}
                    sx={{ alignSelf: "flex-start", textTransform: "none" }}
                  >
                    {showTransferAdvanced ? "Hide advanced" : "Show advanced"}
                  </Button>
                  {showTransferAdvanced && (
                    <TextField
                      size="small"
                      label="Input Holding CIDs (comma-separated)"
                      value={proposalInputHoldingCids}
                      onChange={(e) =>
                        setProposalInputHoldingCids(e.target.value)
                      }
                      fullWidth
                      helperText="Optional — pin specific Holding contracts to spend. Leave empty to let the server select your holdings of the chosen instrument automatically (change is returned)."
                      slotProps={{
                        input: {
                          endAdornment: fieldHelpAdornment(
                            "Optional list of specific Holding contract ids to spend, comma-separated. Leave empty to let the server select your holdings of the chosen instrument automatically; the transfer consumes what it needs and returns change.",
                            "Help for Input Holding CIDs",
                          ),
                        },
                      }}
                    />
                  )}
                </>
              )}

              {proposalType === "accept_transfer" && (
                <Autocomplete
                  size="small"
                  freeSolo
                  options={openTransferInstructions}
                  value={proposalTransferInstructionCid}
                  loading={transferInstructionsLoading}
                  onChange={(_event, value) => {
                    if (typeof value === "string" || value === null) {
                      setProposalTransferInstructionCid(value ?? "");
                    } else {
                      setProposalTransferInstructionCid(value.contract_id);
                    }
                  }}
                  onInputChange={(_event, value, reason) => {
                    // Keep the field in sync when the user types a cid by
                    // hand (freeSolo fallback). `reset` fires when an option
                    // is selected; we already handled that via `onChange`.
                    if (reason === "input") {
                      setProposalTransferInstructionCid(value);
                    }
                  }}
                  getOptionLabel={(option) => {
                    if (typeof option === "string") return option;
                    // Strip the `::1220…` fingerprint suffix from the party
                    // id so the label fits in the dropdown; show a short cid
                    // tail for disambiguation when multiple transfers share
                    // a sender/amount.
                    const senderName = option.sender.split("::")[0];
                    const amount = option.amount.replace(/\.?0+$/, "");
                    const cidTail = option.contract_id.slice(-8);
                    return `${senderName} → ${amount} ${option.instrument_id} (…${cidTail})`;
                  }}
                  getOptionDisabled={(option) => {
                    if (typeof option === "string") return false;
                    if (option.status === "pending_internal_workflow") return true;
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
                    const senderName = option.sender.split("::")[0];
                    const amount = option.amount.replace(/\.?0+$/, "");
                    const cidTail = option.contract_id.slice(-8);
                    const isBlocked =
                      option.status === "pending_internal_workflow";
                    const exp = option.expires_at ?? 0;
                    const isExpired =
                      exp > 0 && exp <= Math.floor(Date.now() / 1000);
                    const pendingSummary = (option.pending_actions ?? [])
                      .map((p) => {
                        const partyName = p.party.split("::")[0];
                        return p.action
                          ? `${partyName} — ${p.action}`
                          : partyName;
                      })
                      .join(", ");
                    return (
                      <li {...props} key={option.contract_id}>
                        <Box
                          sx={{
                            display: "flex",
                            flexDirection: "column",
                            gap: 0.25,
                            opacity: isBlocked || isExpired ? 0.6 : 1,
                          }}
                        >
                          <Typography variant="body2">
                            {senderName} → {amount} {option.instrument_id} (…
                            {cidTail})
                          </Typography>
                          {isExpired && (
                            <Typography variant="caption" color="warning.main">
                              Expired {new Date(exp * 1000).toLocaleString()}
                            </Typography>
                          )}
                          {!isExpired && isBlocked && (
                            <Typography variant="caption" color="warning.main">
                              Waiting on{pendingSummary ? `: ${pendingSummary}` : " internal workflow"}
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
                        <TextHelp text="Contract id of the pending TransferInstruction this party will accept.">
                          TransferInstruction Contract ID
                        </TextHelp>
                      }
                      required
                      helperText={
                        transferInstructionsLoading
                          ? "Loading open transfers…"
                          : openTransferInstructions.length === 0
                            ? "No open transfers visible — paste a contract id directly if you have one"
                            : "Pick an open transfer, or paste a contract id"
                      }
                    />
                  )}
                />
              )}

              {proposalType === "provision_provider_service" && (
                <Typography variant="caption" color="text.secondary">
                  Provisions a Utility-Registry ProviderService with operator = proposer and provider = governance party. No parameters required.
                </Typography>
              )}

              {proposalType === "setup_utility" && (
                <>
                  <FormControl size="small" fullWidth required>
                    <InputLabel>
                      <TextHelp text="ProviderService contract this party received from the operator. Required to set up the registrar.">
                        ProviderService
                      </TextHelp>
                    </InputLabel>
                    <Select
                      label="ProviderService"
                      value={proposalProviderServiceCid}
                      onChange={(e) => setProposalProviderServiceCid(e.target.value)}
                      MenuProps={{ disableScrollLock: true }}
                    >
                      {servicesLoading ? (
                        <MenuItem disabled>Loading services…</MenuItem>
                      ) : providerServices.length > 0 ? (
                        providerServices.map((svc) => (
                          <MenuItem key={svc.contract_id} value={svc.contract_id}>
                            {svc.contract_id}
                          </MenuItem>
                        ))
                      ) : (
                        <MenuItem disabled>
                          No ProviderService found — run "Create Provider Service Request" first
                        </MenuItem>
                      )}
                    </Select>
                  </FormControl>
                  <TextField
                    size="small"
                    label="Operator Party"
                    value={proposalOperator}
                    onChange={(e) => setProposalOperator(e.target.value)}
                    fullWidth
                    required
                    slotProps={{
                      input: {
                        endAdornment: fieldHelpAdornment(
                          "Party id of the utility operator that issued the ProviderService.",
                          "Help for Operator Party",
                        ),
                      },
                    }}
                  />
                  <TextField
                    size="small"
                    label="Instrument ID"
                    value={proposalInstrumentIdText}
                    onChange={(e) => setProposalInstrumentIdText(e.target.value)}
                    fullWidth
                    required
                    slotProps={{
                      input: {
                        endAdornment: fieldHelpAdornment(
                          "Token name for the instrument this utility will mint and burn (e.g. \"cTM\"). The governance party is the issuer.",
                          "Help for Instrument ID",
                        ),
                      },
                    }}
                  />
                  <FormControlLabel
                    control={<Checkbox size="small" checked={proposalCreateTransferRule} onChange={(e) => setProposalCreateTransferRule(e.target.checked)} />}
                    label={
                      <TextHelp text="Also create a TransferRule contract so holders can transfer this token without per-transfer governance.">
                        Create TransferRule
                      </TextHelp>
                    }
                  />
                  <FormControlLabel
                    control={<Checkbox size="small" checked={proposalCreateAllocationFactory} onChange={(e) => setProposalCreateAllocationFactory(e.target.checked)} />}
                    label={
                      <TextHelp text="Also create an AllocationFactory contract so this token can be allocated by external apps.">
                        Create AllocationFactory
                      </TextHelp>
                    }
                  />
                </>
              )}

              {proposalType === "create_provider_service_request" && (
                <>
                  {providerServices.length > 0 && (
                    <Alert severity="warning">
                      This party already has {providerServices.length} ProviderService
                      contract{providerServices.length === 1 ? "" : "s"}; creating
                      another request will fail when executed.
                    </Alert>
                  )}
                  <TextField
                    size="small"
                    label="Operator Party"
                    value={proposalOperator}
                    onChange={(e) => setProposalOperator(e.target.value)}
                    fullWidth
                    required
                    slotProps={{
                      input: {
                        endAdornment: fieldHelpAdornment(
                          "Party id of the utility operator that will receive and sign off on this request.",
                          "Help for Operator Party",
                        ),
                      },
                    }}
                  />
                  <TextField
                    size="small"
                    label="Provider Party"
                    value={proposalProvider}
                    onChange={(e) => setProposalProvider(e.target.value)}
                    fullWidth
                    required
                    slotProps={{
                      input: {
                        endAdornment: fieldHelpAdornment(
                          "Party id that wants to become a provider on the utility (usually this governance party).",
                          "Help for Provider Party",
                        ),
                      },
                    }}
                  />
                </>
              )}

              {proposalType === "create_user_service_request" && (
                <>
                  {userServices.length > 0 && (
                    <Alert severity="warning">
                      This party already has {userServices.length} UserService
                      contract{userServices.length === 1 ? "" : "s"}; creating
                      another request will fail when executed.
                    </Alert>
                  )}
                  <TextField
                    size="small"
                    label="Operator Party"
                    value={proposalOperator}
                    onChange={(e) => setProposalOperator(e.target.value)}
                    fullWidth
                    required
                    slotProps={{
                      input: {
                        endAdornment: fieldHelpAdornment(
                          "Party id of the utility operator that will receive and sign off on this request.",
                          "Help for Operator Party",
                        ),
                      },
                    }}
                  />
                  <TextField
                    size="small"
                    label="User Party"
                    value={proposalUser}
                    onChange={(e) => setProposalUser(e.target.value)}
                    fullWidth
                    required
                    slotProps={{
                      input: {
                        endAdornment: fieldHelpAdornment(
                          "Party id that wants to become a user of the utility (usually this governance party).",
                          "Help for User Party",
                        ),
                      },
                    }}
                  />
                </>
              )}

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
                  <TextField
                    label="DSO Party"
                    value={proposalDelegationDso}
                    onChange={(e) => setProposalDelegationDso(e.target.value)}
                    fullWidth
                    required
                    slotProps={{
                      input: {
                        endAdornment: fieldHelpAdornment(
                          "The DSO whose coupons this delegation may assign. Anyone can mint a coupon naming themselves DSO, so the automation ignores every coupon whose DSO is not this one. Getting it wrong silently assigns nothing.",
                          "Help for DSO Party",
                        ),
                      },
                    }}
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
                          input: { sx: { fontFamily: "monospace", fontSize: "0.8rem" } },
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
                                input: { sx: { fontFamily: "monospace", fontSize: "0.8rem" } },
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
                              <TextField
                                label="Weight"
                                value={b.weight}
                                onChange={(e) => {
                                  const updated = [...proposalDelegationSplit];
                                  updated[idx] = { ...b, weight: e.target.value };
                                  setProposalDelegationSplit(updated);
                                }}
                                size="small"
                                sx={{ width: 110 }}
                                slotProps={{
                                  input: {
                                    endAdornment: fieldHelpAdornment(
                                      "A whole number. Only the ratio matters: 1/1/1 is equal thirds, 80/20 is four to one. The exact percentage is derived.",
                                      "Help for Weight",
                                    ),
                                  },
                                }}
                              />
                              <Typography
                                variant="caption"
                                sx={{ flex: 1, fontFamily: "monospace" }}
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

              {proposalType === "set_provider_app_reward_beneficiaries" && (
                <>
                  <FormControl size="small" fullWidth required>
                    <InputLabel>
                      <TextHelp text="Which utility-issued instrument these beneficiaries apply to.">
                        InstrumentConfiguration
                      </TextHelp>
                    </InputLabel>
                    <Select
                      label="InstrumentConfiguration"
                      value={proposalInstrumentConfigurationCid}
                      onChange={(e) => setProposalInstrumentConfigurationCid(e.target.value)}
                      MenuProps={{ disableScrollLock: true }}
                    >
                      {instrumentsLoading ? (
                        <MenuItem disabled>Loading instruments…</MenuItem>
                      ) : availableInstruments.length > 0 ? (
                        availableInstruments.map((inst) => (
                          <MenuItem key={inst.contract_id} value={inst.contract_id}>
                            {inst.instrument_id} ({inst.contract_id.slice(0, 8)}…)
                          </MenuItem>
                        ))
                      ) : (
                        <MenuItem disabled>
                          No instruments found — run SetupUtility first
                        </MenuItem>
                      )}
                    </Select>
                  </FormControl>
                  <FormControlLabel
                    control={<Checkbox size="small" checked={proposalClearBeneficiaries} onChange={(e) => setProposalClearBeneficiaries(e.target.checked)} />}
                    label={
                      <TextHelp text="Check this to remove all beneficiaries instead of setting a new list.">
                        Clear beneficiaries (set to None)
                      </TextHelp>
                    }
                  />
                  {!proposalClearBeneficiaries && (
                    <>
                      <Typography
                        variant="caption"
                        color="text.secondary"
                        sx={{ display: "block" }}
                      >
                        <TextHelp text="Parties that share this instrument's app rewards. Each row is a party plus a weight; weights are decimals and must sum to 1.0.">
                          Beneficiaries (add party + weight)
                        </TextHelp>
                      </Typography>
                      {proposalBeneficiaries.map((b, idx) => (
                        <Box
                          key={idx}
                          sx={{ display: "flex", gap: 1, mb: 1 }}
                        >
                          <TextField
                            label="Beneficiary Party"
                            value={b.beneficiary}
                            onChange={(e) => {
                              const updated = [...proposalBeneficiaries];
                              updated[idx] = {
                                ...b,
                                beneficiary: e.target.value,
                              };
                              setProposalBeneficiaries(updated);
                            }}
                            size="small"
                            sx={{ flex: 2 }}
                            slotProps={{
                              input: {
                                endAdornment: fieldHelpAdornment(
                                  "Party id that receives this share of the instrument's app rewards.",
                                  "Help for Beneficiary Party",
                                ),
                              },
                            }}
                          />
                          <TextField
                            label="Weight"
                            value={b.weight}
                            onChange={(e) => {
                              const updated = [...proposalBeneficiaries];
                              updated[idx] = { ...b, weight: e.target.value };
                              setProposalBeneficiaries(updated);
                            }}
                            size="small"
                            sx={{ flex: 1 }}
                            slotProps={{
                              input: {
                                endAdornment: fieldHelpAdornment(
                                  "Fraction of rewards this beneficiary gets, as a decimal. All row weights must sum to 1.0.",
                                  "Help for Weight",
                                ),
                              },
                            }}
                          />
                          <Button
                            size="small"
                            color="error"
                            onClick={() =>
                              setProposalBeneficiaries(
                                proposalBeneficiaries.filter(
                                  (_, i) => i !== idx,
                                ),
                              )
                            }
                          >
                            Remove
                          </Button>
                        </Box>
                      ))}
                      <Box
                        sx={{
                          display: "flex",
                          alignItems: "center",
                          gap: 2,
                        }}
                      >
                        <Button
                          size="small"
                          onClick={() =>
                            setProposalBeneficiaries([
                              ...proposalBeneficiaries,
                              { beneficiary: "", weight: "1" },
                            ])
                          }
                        >
                          Add Beneficiary
                        </Button>
                        {proposalBeneficiaries.length > 0 &&
                          (() => {
                            const sum = proposalBeneficiaries.reduce(
                              (acc, b) => acc + (parseFloat(b.weight) || 0),
                              0,
                            );
                            const isValid = Math.abs(sum - 1.0) < 1e-9;
                            return (
                              <Typography
                                variant="caption"
                                color={
                                  isValid ? "success.main" : "error.main"
                                }
                              >
                                Sum: {sum.toFixed(4)}{" "}
                                {isValid ? "" : "(must be 1.0)"}
                              </Typography>
                            );
                          })()}
                      </Box>
                    </>
                  )}
                </>
              )}

              {proposalType === "set_enable_result_contracts" && (
                <>
                  <FormControl size="small" fullWidth required>
                    <InputLabel>
                      <TextHelp text="RegistrarService contract whose result-contract setting will be updated.">
                        RegistrarService
                      </TextHelp>
                    </InputLabel>
                    <Select
                      label="RegistrarService"
                      value={proposalRegistrarServiceCid}
                      onChange={(e) => setProposalRegistrarServiceCid(e.target.value)}
                      MenuProps={{ disableScrollLock: true }}
                    >
                      {registrarServiceContracts.length > 0 ? (
                        registrarServiceContracts.map((c) => (
                          <MenuItem key={c.contract_id} value={c.contract_id}>
                            {c.contract_id}
                          </MenuItem>
                        ))
                      ) : (
                        <MenuItem disabled>
                          No RegistrarService found — run SetupUtility first
                        </MenuItem>
                      )}
                    </Select>
                  </FormControl>
                  <FormControl size="small" fullWidth>
                    <InputLabel>
                      <TextHelp text="Whether the registrar should emit result contracts after operations. Clear sets the value back to None.">
                        Enable Result Contracts
                      </TextHelp>
                    </InputLabel>
                    <Select
                      label="Enable Result Contracts"
                      value={proposalEnableResultContracts}
                      onChange={(e) => setProposalEnableResultContracts(e.target.value as "true" | "false" | "clear")}
                    >
                      <MenuItem value="true">Enable</MenuItem>
                      <MenuItem value="false">Disable</MenuItem>
                      <MenuItem value="clear">Clear (None)</MenuItem>
                    </Select>
                  </FormControl>
                </>
              )}

              {proposalType === "create_delegated_batched_markers_proxy" && (
                <TextField
                  size="small"
                  label="Operator Party"
                  value={proposalOperator}
                  onChange={(e) => setProposalOperator(e.target.value)}
                  fullWidth
                  required
                  slotProps={{
                    input: {
                      endAdornment: fieldHelpAdornment(
                        "Party id of the utility operator that will own the delegated batched markers proxy.",
                        "Help for Operator Party",
                      ),
                    },
                  }}
                />
              )}

              {proposalType === "setup_minting_delegation" && (
                <>
                  <TextField
                    size="small"
                    label="Delegate Party"
                    value={proposalDelegate}
                    onChange={(e) => setProposalDelegate(e.target.value)}
                    fullWidth
                    required
                    slotProps={{
                      input: {
                        endAdornment: fieldHelpAdornment(
                          "Party id of the validator node operator authorized to mint the decentralized party's reward coupons. It accepts the resulting MintingDelegationProposal out-of-band via the wallet API.",
                          "Help for Delegate Party",
                        ),
                      },
                    }}
                  />
                  <TextField
                    size="small"
                    label="DSO Party"
                    value={proposalExpectedDso}
                    onChange={(e) => setProposalExpectedDso(e.target.value)}
                    fullWidth
                    required
                    slotProps={{
                      input: {
                        endAdornment: fieldHelpAdornment(
                          "Party id of the Splice DSO; the delegation's mint transfers verify the AmuletRules contract belongs to this DSO.",
                          "Help for DSO Party",
                        ),
                      },
                    }}
                  />
                  <TextField
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
                      input: {
                        endAdornment: fieldHelpAdornment(
                          "When the delegation stops being valid. There is no auto-renewal — a new proposal must be voted in before this time to keep collecting rewards.",
                          "Help for Expires At",
                        ),
                      },
                    }}
                  />
                  <TextField
                    size="small"
                    label="Amulet Merge Limit"
                    type="number"
                    value={proposalAmuletMergeLimit}
                    onChange={(e) => setProposalAmuletMergeLimit(e.target.value)}
                    fullWidth
                    required
                    slotProps={{
                      input: {
                        endAdornment: fieldHelpAdornment(
                          "Number of amulet contracts to keep after auto-merging; the delegate merges contracts once there are strictly more than this number. Must be positive.",
                          "Help for Amulet Merge Limit",
                        ),
                      },
                    }}
                  />
                  <TextField
                    size="small"
                    label="Description"
                    value={proposalDescription}
                    onChange={(e) => setProposalDescription(e.target.value)}
                    fullWidth
                    required
                    slotProps={{
                      input: {
                        endAdornment: fieldHelpAdornment(
                          "Free-form note recorded on the proposal, e.g. why this delegation is being set up.",
                          "Help for Description",
                        ),
                      },
                    }}
                  />
                </>
              )}

              {proposalType === "accept_external_party_setup" && (
                <TextField
                  size="small"
                  label="External Party Setup Proposal Contract Id"
                  value={proposalExternalPartySetupCid}
                  onChange={(e) =>
                    setProposalExternalPartySetupCid(e.target.value)
                  }
                  fullWidth
                  required
                  slotProps={{
                    input: {
                      endAdornment: fieldHelpAdornment(
                        "Contract id of the ExternalPartySetupProposal the validator operator created (via POST /api/validator/v0/admin/external-party/setup-proposal). Accepting it creates the decentralized party's ValidatorRight + TransferPreapproval, unblocking reward collection.",
                        "Help for External Party Setup Proposal Contract Id",
                      ),
                    },
                  }}
                />
              )}

              {(proposalType === "mint" || proposalType === "burn") && (
                <>
                  <FormControl size="small" fullWidth required>
                    <InputLabel>
                      <TextHelp
                        text={
                          proposalType === "mint"
                            ? "Instrument being minted. The governance party is the issuer."
                            : "Instrument being burned. The governance party is the issuer."
                        }
                      >
                        Instrument
                      </TextHelp>
                    </InputLabel>
                    <Select
                      label="Instrument"
                      value={proposalInstrumentConfigurationCid}
                      onChange={(e) => {
                        const cid = e.target.value;
                        const inst = availableInstruments.find(
                          (i) => i.contract_id === cid,
                        );
                        setProposalInstrumentConfigurationCid(cid);
                        // instrument_admin is always the decparty (seeded by
                        // the effect above) — only `id` comes from the picked
                        // instrument.
                        if (inst) {
                          setProposalInstrumentIdId(inst.instrument_id);
                        }
                      }}
                      MenuProps={{ disableScrollLock: true }}
                    >
                      {instrumentsLoading ? (
                        <MenuItem disabled>Loading instruments…</MenuItem>
                      ) : availableInstruments.length > 0 ? (
                        availableInstruments.map((inst) => (
                          <MenuItem key={inst.contract_id} value={inst.contract_id}>
                            {inst.instrument_id} ({inst.contract_id.slice(0, 8)}…)
                          </MenuItem>
                        ))
                      ) : (
                        <MenuItem disabled>
                          No instruments found — run SetupUtility first
                        </MenuItem>
                      )}
                    </Select>
                  </FormControl>
                  <FormControl size="small" fullWidth required>
                    <InputLabel>
                      <TextHelp text="AllocationFactory contract used to allocate the new holding.">
                        AllocationFactory
                      </TextHelp>
                    </InputLabel>
                    <Select
                      label="AllocationFactory"
                      value={proposalAllocationFactoryCid}
                      onChange={(e) => setProposalAllocationFactoryCid(e.target.value)}
                      MenuProps={{ disableScrollLock: true }}
                    >
                      {allocationFactoryContracts.length > 0 ? (
                        allocationFactoryContracts.map((c) => (
                          <MenuItem key={c.contract_id} value={c.contract_id}>
                            {c.contract_id}
                          </MenuItem>
                        ))
                      ) : (
                        <MenuItem disabled>
                          No AllocationFactory found — run SetupUtility first
                        </MenuItem>
                      )}
                    </Select>
                  </FormControl>
                  <TextField
                    size="small"
                    label={proposalType === "mint" ? "Recipient Party" : "Holder Party"}
                    value={proposalType === "mint" ? proposalRecipient : proposalHolder}
                    onChange={(e) => proposalType === "mint" ? setProposalRecipient(e.target.value) : setProposalHolder(e.target.value)}
                    fullWidth
                    required
                    slotProps={{
                      input: {
                        endAdornment: fieldHelpAdornment(
                          proposalType === "mint"
                            ? "Party id that will receive the newly minted tokens."
                            : "Party id whose tokens will be burned.",
                          proposalType === "mint" ? "Help for Recipient Party" : "Help for Holder Party",
                        ),
                      },
                    }}
                  />
                  <TextField
                    size="small"
                    label="Amount"
                    value={proposalAmount}
                    onChange={(e) => setProposalAmount(e.target.value)}
                    fullWidth
                    required
                    slotProps={{
                      input: {
                        endAdornment: fieldHelpAdornment(
                          proposalType === "mint"
                            ? "How much of the selected instrument to mint."
                            : "How much of the selected instrument to burn.",
                          "Help for Amount",
                        ),
                      },
                    }}
                  />
                  <TextField
                    size="small"
                    label="Description"
                    value={proposalDescription}
                    onChange={(e) => setProposalDescription(e.target.value)}
                    fullWidth
                    required
                    slotProps={{
                      input: {
                        endAdornment: fieldHelpAdornment(
                          "Free-form human-readable note explaining why this mint or burn is being proposed.",
                          "Help for Description",
                        ),
                      },
                    }}
                  />
                </>
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
                  if (inst) setProposalInstrumentConfigurationCid(inst.contract_id);
                };
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
                    <FormControl size="small" fullWidth required disabled>
                      <InputLabel>
                        <TextHelp text="Instrument the request was made against. Derived automatically from the selected request.">
                          Instrument
                        </TextHelp>
                      </InputLabel>
                      <Select
                        label="Instrument"
                        value={proposalInstrumentConfigurationCid}
                        onChange={(e) => setProposalInstrumentConfigurationCid(e.target.value)}
                        MenuProps={{ disableScrollLock: true }}
                      >
                        {instrumentsLoading ? (
                          <MenuItem disabled>Loading instruments…</MenuItem>
                        ) : availableInstruments.length > 0 ? (
                          availableInstruments.map((inst) => (
                            <MenuItem key={inst.contract_id} value={inst.contract_id}>
                              {inst.instrument_id} ({inst.contract_id.slice(0, 8)}…)
                            </MenuItem>
                          ))
                        ) : (
                          <MenuItem disabled>
                            No instruments found — run SetupUtility first
                          </MenuItem>
                        )}
                      </Select>
                    </FormControl>
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
                    <TextField
                      size="small"
                      label="Description"
                      value={proposalDescription}
                      onChange={(e) => setProposalDescription(e.target.value)}
                      fullWidth
                      required
                      slotProps={{
                        input: {
                          endAdornment: fieldHelpAdornment(
                            "Free-form human-readable note explaining why this request is being accepted.",
                            "Help for Description",
                          ),
                        },
                      }}
                    />
                  </>
                );
              })()}

              {proposalType === "offer_free_credential" && (
                <>
                  <FormControl size="small" fullWidth required>
                    <InputLabel>
                      <TextHelp text="Contract id of this governance party's UserService, used to issue the credential offer.">
                        UserService Contract ID
                      </TextHelp>
                    </InputLabel>
                    <Select
                      label="UserService Contract ID"
                      value={proposalUserServiceCid}
                      onChange={(e) => setProposalUserServiceCid(e.target.value)}
                      MenuProps={{ disableScrollLock: true }}
                    >
                      {servicesLoading ? (
                        <MenuItem disabled>Loading services…</MenuItem>
                      ) : userServices.length > 0 ? (
                        userServices.map((svc) => (
                          <MenuItem key={svc.contract_id} value={svc.contract_id}>
                            {svc.contract_id}
                          </MenuItem>
                        ))
                      ) : (
                        <MenuItem disabled>No user services found</MenuItem>
                      )}
                    </Select>
                  </FormControl>
                  <TextField
                    size="small"
                    label="Holder Party"
                    value={proposalHolder}
                    onChange={(e) => setProposalHolder(e.target.value)}
                    fullWidth
                    required
                    slotProps={{
                      input: {
                        endAdornment: fieldHelpAdornment(
                          "Party id that will receive the free credential offer.",
                          "Help for Holder Party",
                        ),
                      },
                    }}
                  />
                  <TextField
                    size="small"
                    label="Credential ID"
                    value={proposalCredentialId}
                    onChange={(e) => setProposalCredentialId(e.target.value)}
                    fullWidth
                    required
                    slotProps={{
                      input: {
                        endAdornment: fieldHelpAdornment(
                          "Unique identifier for this credential (free-form string).",
                          "Help for Credential ID",
                        ),
                      },
                    }}
                  />
                  <TextField
                    size="small"
                    label="Description"
                    value={proposalDescription}
                    onChange={(e) => setProposalDescription(e.target.value)}
                    fullWidth
                    required
                    slotProps={{
                      input: {
                        endAdornment: fieldHelpAdornment(
                          "Free-form human-readable description of what this credential certifies.",
                          "Help for Description",
                        ),
                      },
                    }}
                  />
                  <TextField
                    size="small"
                    label="Claims (one per line: subject,property,value)"
                    value={proposalCredentialClaimsText}
                    onChange={(e) => setProposalCredentialClaimsText(e.target.value)}
                    fullWidth
                    multiline
                    minRows={2}
                    maxRows={6}
                    helperText='Each line: "<subject>,<property>,<value>"'
                    slotProps={{
                      input: {
                        endAdornment: fieldHelpAdornment(
                          "Claims baked into the credential. One per line, each formatted as subject,property,value.",
                          "Help for Claims",
                        ),
                      },
                    }}
                  />
                </>
              )}

              {proposalType === "accept_free_credential" && (
                <>
                  <FormControl size="small" fullWidth required>
                    <InputLabel>
                      <TextHelp text="Contract id of this governance party's UserService, used to accept the credential offer.">
                        UserService Contract ID
                      </TextHelp>
                    </InputLabel>
                    <Select
                      label="UserService Contract ID"
                      value={proposalUserServiceCid}
                      onChange={(e) => setProposalUserServiceCid(e.target.value)}
                      MenuProps={{ disableScrollLock: true }}
                    >
                      {servicesLoading ? (
                        <MenuItem disabled>Loading services…</MenuItem>
                      ) : userServices.length > 0 ? (
                        userServices.map((svc) => (
                          <MenuItem key={svc.contract_id} value={svc.contract_id}>
                            {svc.contract_id}
                          </MenuItem>
                        ))
                      ) : (
                        <MenuItem disabled>No user services found</MenuItem>
                      )}
                    </Select>
                  </FormControl>
                  {renderCredentialOfferAutocomplete(
                    proposalCredentialOfferCid,
                    setProposalCredentialOfferCid,
                  )}
                </>
              )}

              {proposalType === "create_provider_configuration" && (
                <>
                  <FormControl size="small" fullWidth required>
                    <InputLabel>
                      <TextHelp text="ProviderService contract of this provider decparty. Its provider must be the governance party.">
                        ProviderService
                      </TextHelp>
                    </InputLabel>
                    <Select
                      label="ProviderService"
                      value={proposalProviderServiceCid}
                      onChange={(e) => setProposalProviderServiceCid(e.target.value)}
                      MenuProps={{ disableScrollLock: true }}
                    >
                      {servicesLoading ? (
                        <MenuItem disabled>Loading services…</MenuItem>
                      ) : providerServices.length > 0 ? (
                        providerServices.map((svc) => (
                          <MenuItem key={svc.contract_id} value={svc.contract_id}>
                            {svc.contract_id}
                          </MenuItem>
                        ))
                      ) : (
                        <MenuItem disabled>
                          No ProviderService found — run "Create Provider Service Request" first
                        </MenuItem>
                      )}
                    </Select>
                  </FormControl>
                  {renderRequirementRows(
                    "Registrar Requirements",
                    "Credential requirements a registrar must meet before the provider onboards it. Rows whose issuer is this governance party are minted automatically during Onboard Registrar.",
                    proposalRegistrarRequirements,
                    setProposalRegistrarRequirements,
                  )}
                  {renderRequirementRows(
                    "Holder Requirements",
                    "Credential requirements a token holder must meet on this provider's utility.",
                    proposalHolderRequirements,
                    setProposalHolderRequirements,
                  )}
                </>
              )}

              {proposalType === "create_registrar_service_request" && (
                <>
                  <TextField
                    size="small"
                    label="Operator Party"
                    value={proposalOperator}
                    onChange={(e) => setProposalOperator(e.target.value)}
                    fullWidth
                    required
                    slotProps={{
                      input: {
                        endAdornment: fieldHelpAdornment(
                          "Party id of the utility operator the registrar service runs under.",
                          "Help for Operator Party",
                        ),
                      },
                    }}
                  />
                  <TextField
                    size="small"
                    label="Provider Party"
                    value={proposalProvider}
                    onChange={(e) => setProposalProvider(e.target.value)}
                    fullWidth
                    required
                    slotProps={{
                      input: {
                        endAdornment: fieldHelpAdornment(
                          "Party id of the provider decparty this request asks for registrar service. The provider accepts via Onboard Registrar.",
                          "Help for Provider Party",
                        ),
                      },
                    }}
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
                  <FormControl size="small" fullWidth required>
                    <InputLabel>
                      <TextHelp text="ProviderService contract of this provider decparty, used to accept the request.">
                        ProviderService
                      </TextHelp>
                    </InputLabel>
                    <Select
                      label="ProviderService"
                      value={proposalProviderServiceCid}
                      onChange={(e) => setProposalProviderServiceCid(e.target.value)}
                      MenuProps={{ disableScrollLock: true }}
                    >
                      {servicesLoading ? (
                        <MenuItem disabled>Loading services…</MenuItem>
                      ) : providerServices.length > 0 ? (
                        providerServices.map((svc) => (
                          <MenuItem key={svc.contract_id} value={svc.contract_id}>
                            {svc.contract_id}
                          </MenuItem>
                        ))
                      ) : (
                        <MenuItem disabled>
                          No ProviderService found — run "Create Provider Service Request" first
                        </MenuItem>
                      )}
                    </Select>
                  </FormControl>
                  <FormControl size="small" fullWidth required>
                    <InputLabel>
                      <TextHelp text="Pending RegistrarServiceRequest to accept. The request names the registrar to onboard; its provider must be this governance party.">
                        RegistrarServiceRequest
                      </TextHelp>
                    </InputLabel>
                    <Select
                      label="RegistrarServiceRequest"
                      value={proposalRegistrarServiceRequestCid}
                      onChange={(e) => setProposalRegistrarServiceRequestCid(e.target.value)}
                      MenuProps={{ disableScrollLock: true }}
                    >
                      {registrarServiceRequestsLoading ? (
                        <MenuItem disabled>Loading requests…</MenuItem>
                      ) : acceptableRegistrarServiceRequests.length > 0 ? (
                        acceptableRegistrarServiceRequests.map((req) => (
                          <MenuItem key={req.contract_id} value={req.contract_id}>
                            {req.registrar.split("::")[0]} (…{req.contract_id.slice(-8)})
                          </MenuItem>
                        ))
                      ) : (
                        <MenuItem disabled>
                          No pending requests for this provider — the registrar runs "Create Registrar Service Request" first
                        </MenuItem>
                      )}
                    </Select>
                  </FormControl>
                  <FormControl size="small" fullWidth required>
                    <InputLabel>
                      <TextHelp text="ProviderConfiguration holding the registrar requirements to mint and validate against.">
                        ProviderConfiguration
                      </TextHelp>
                    </InputLabel>
                    <Select
                      label="ProviderConfiguration"
                      value={proposalProviderConfigurationCid}
                      onChange={(e) => setProposalProviderConfigurationCid(e.target.value)}
                      MenuProps={{ disableScrollLock: true }}
                    >
                      {providerConfigurationsLoading ? (
                        <MenuItem disabled>Loading configurations…</MenuItem>
                      ) : providerConfigurations.length > 0 ? (
                        providerConfigurations.map((cfg) => (
                          <MenuItem key={cfg.contract_id} value={cfg.contract_id}>
                            {cfg.contract_id}
                          </MenuItem>
                        ))
                      ) : (
                        <MenuItem disabled>
                          No ProviderConfiguration found — run "Create Provider Configuration" first
                        </MenuItem>
                      )}
                    </Select>
                  </FormControl>
                </>
              )}

              {proposalType === "provision_instrument" && (
                <>
                  <FormControl size="small" fullWidth required>
                    <InputLabel>
                      <TextHelp text="RegistrarService contract of this registrar decparty, used to create the InstrumentConfiguration.">
                        RegistrarService
                      </TextHelp>
                    </InputLabel>
                    <Select
                      label="RegistrarService"
                      value={proposalRegistrarServiceCid}
                      onChange={(e) => setProposalRegistrarServiceCid(e.target.value)}
                      MenuProps={{ disableScrollLock: true }}
                    >
                      {registrarServicesLoading ? (
                        <MenuItem disabled>Loading services…</MenuItem>
                      ) : ownRegistrarServices.length > 0 ? (
                        ownRegistrarServices.map((svc) => (
                          <MenuItem key={svc.contract_id} value={svc.contract_id}>
                            {svc.contract_id}
                          </MenuItem>
                        ))
                      ) : (
                        <MenuItem disabled>
                          No RegistrarService for this party — run "Onboard Registrar" first
                        </MenuItem>
                      )}
                    </Select>
                  </FormControl>
                  <TextField
                    size="small"
                    label="Instrument ID"
                    value={proposalInstrumentIdText}
                    onChange={(e) => setProposalInstrumentIdText(e.target.value)}
                    fullWidth
                    required
                    slotProps={{
                      input: {
                        endAdornment: fieldHelpAdornment(
                          "Token name for the instrument this registrar will manage (e.g. \"cTM\").",
                          "Help for Instrument ID",
                        ),
                      },
                    }}
                  />
                  {renderRequirementRows(
                    "Issuer Requirements",
                    "Credential requirements an instrument issuer must meet. Rows whose issuer is this governance party are minted automatically for the initial issuers below.",
                    proposalIssuerRequirements,
                    setProposalIssuerRequirements,
                  )}
                  {renderRequirementRows(
                    "Holder Requirements",
                    "Credential requirements a holder of this instrument must meet.",
                    proposalHolderRequirements,
                    setProposalHolderRequirements,
                  )}
                  <TextField
                    size="small"
                    label="Initial Instrument Issuers (one party id per line)"
                    value={proposalInitialInstrumentIssuersText}
                    onChange={(e) => setProposalInitialInstrumentIssuersText(e.target.value)}
                    fullWidth
                    multiline
                    minRows={2}
                    maxRows={6}
                    slotProps={{
                      input: {
                        endAdornment: fieldHelpAdornment(
                          "Parties credentialed as instrument issuers at provisioning, one party id per line. Leave empty to onboard issuers later.",
                          "Help for Initial Instrument Issuers",
                        ),
                      },
                    }}
                  />
                </>
              )}

              {proposalType === "onboard_instrument_issuers" && (
                <>
                  <FormControl size="small" fullWidth required>
                    <InputLabel>
                      <TextHelp text="InstrumentConfiguration whose issuer requirements the new issuers are credentialed against.">
                        InstrumentConfiguration
                      </TextHelp>
                    </InputLabel>
                    <Select
                      label="InstrumentConfiguration"
                      value={proposalInstrumentConfigurationCid}
                      onChange={(e) => setProposalInstrumentConfigurationCid(e.target.value)}
                      MenuProps={{ disableScrollLock: true }}
                    >
                      {instrumentsLoading ? (
                        <MenuItem disabled>Loading instruments…</MenuItem>
                      ) : ownInstruments.length > 0 ? (
                        ownInstruments.map((inst) => (
                          <MenuItem key={inst.contract_id} value={inst.contract_id}>
                            {inst.instrument_id} ({inst.contract_id.slice(0, 8)}…)
                          </MenuItem>
                        ))
                      ) : (
                        <MenuItem disabled>
                          No instruments administered by this party — run "Provision Instrument" first
                        </MenuItem>
                      )}
                    </Select>
                  </FormControl>
                  <TextField
                    size="small"
                    label="Instrument Issuers (one party id per line)"
                    value={proposalInstrumentIssuersText}
                    onChange={(e) => setProposalInstrumentIssuersText(e.target.value)}
                    fullWidth
                    required
                    multiline
                    minRows={2}
                    maxRows={6}
                    slotProps={{
                      input: {
                        endAdornment: fieldHelpAdornment(
                          "Parties to credential as instrument issuers, one party id per line. At least one is required.",
                          "Help for Instrument Issuers",
                        ),
                      },
                    }}
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
                      <TextField
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
                        slotProps={{
                          input: {
                            endAdornment: fieldHelpAdornment(
                              "Party id of the issuer being offboarded. The credential list below filters to credentials whose claims all name this party.",
                              "Help for Instrument Issuer Party",
                            ),
                          },
                        }}
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

              {proposalType === "offer_paid_credential" && (
                <Typography variant="caption" color="text.secondary">
                  Paid credential proposal form is not implemented yet. Use the Free direction or call <code>POST /governance/propose</code> directly with a <code>type: "offer_paid_credential"</code> payload.
                </Typography>
              )}

              {(() => {
                const inlineSubmitBtn = (
                  <Button
                    variant="contained"
                    onClick={handleSubmitProposal}
                    disabled={
                      proposalLoading ||
                      proposalType === "offer_paid_credential"
                    }
                    startIcon={
                      proposalLoading ? (
                        <CircularProgress size={16} />
                      ) : (
                        <CheckCircleIcon />
                      )
                    }
                  >
                    Submit Proposal
                  </Button>
                );
                const portalSubmitBtn = (
                  <Button
                    onClick={handleSubmitProposal}
                    disabled={
                      proposalLoading ||
                      proposalType === "offer_paid_credential"
                    }
                    startIcon={
                      proposalLoading ? <CircularProgress size={16} /> : undefined
                    }
                  >
                    Submit Proposal
                  </Button>
                );
                return submitPortalEl ? (
                  <Portal container={submitPortalEl}>{portalSubmitBtn}</Portal>
                ) : (
                  inlineSubmitBtn
                );
              })()}
            </Box>
          </Collapse>
        </Box>
      )}

    </Box>
  );
};
