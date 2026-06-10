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
  FormGroup,
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
  MAX_TOTAL_DEPOSIT,
  MIN_DEPOSIT_AMOUNT,
  MIN_WITHDRAWAL_AMOUNT,
  DEVNET_VAULT_RULES,
  DEVNET_VAULT_BACKEND_SIGNATORY,
  DEVNET_CBTC_DEC_PARTY,
  DEVNET_VAULT_PROCESSOR_RULES,
  TEMPLATE_VAULT_RULES,
  TEMPLATE_ALLOCATION_FACTORY,
  INTERFACE_FEATURED_APP_RIGHT,
  TEMPLATE_REGISTRAR_SERVICE,
} from "../constants";
import { authenticatedFetch } from "../api";
import { getActionTypeOptions } from "../governanceFormat";
import { fieldHelpAdornment, TextHelp } from "./FieldHelp";
import type {
  GovernanceResponse,
  ActionType,
  ConfirmActionRequest,
  InstrumentId,
  VaultLimits,
  FarConfig,
  AppRewardBeneficiary,
  Claim,
  VaultInfo,
  VaultsResponse,
  ProviderServiceInfo,
  ProviderServicesResponse,
  UserServiceInfo,
  UserServicesResponse,
  CredentialOfferInfo,
  CredentialOffersResponse,
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
} from "../types";

type ActionTypeKey = ActionType["type"];

interface GovernanceSectionProps {
  partyId: string;
  rulesContractId?: string;
  governanceContractIds?: string[];
  defaultOperatorParty?: string;
  network?: Network;
  governanceType?: "vault" | "core_self" | "core_domain";
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

// Default values for action form
const defaultVaultName = "cbtc-vault-v0-rc1";
const defaultShareSymbol = "CBTCV0RC1";
const defaultInstrumentId: InstrumentId = {
  admin: DEVNET_CBTC_DEC_PARTY,
  id: "CBTC",
};
const defaultVaultLimits: VaultLimits = {
  max_total_deposit: MAX_TOTAL_DEPOSIT.toString(),
  min_deposit_amount: MIN_DEPOSIT_AMOUNT.toString(),
  min_withdrawal_amount: MIN_WITHDRAWAL_AMOUNT.toString(),
};
const defaultFarConfig: FarConfig = {
  featured_app_right_cid: "",
  beneficiaries: [],
};
const defaultVaultRulesCid = DEVNET_VAULT_RULES.contract_id;
const defaultVaultBackendSignatory = DEVNET_VAULT_BACKEND_SIGNATORY;

export const GovernanceSection = ({
  partyId,
  rulesContractId: initialRulesContractId,
  governanceContractIds = [],
  defaultOperatorParty,
  network,
  governanceType = "vault",
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
  // Row-based beneficiary entry, same shape as the vault FAR beneficiaries
  // form. Each row is { beneficiary, weight } — weights are decimals that
  // must sum to 1.0 (validated client-side + by Daml on submit).
  const [proposalBeneficiaries, setProposalBeneficiaries] = useState<
    { beneficiary: string; weight: string }[]
  >([]);
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
  const [vaultName, setVaultName] = useState(defaultVaultName);
  const [shareSymbol, setShareSymbol] = useState(defaultShareSymbol);
  const [assetInstrumentId, setAssetInstrumentId] =
    useState<InstrumentId>(defaultInstrumentId);
  const [vaultLimits, setVaultLimits] =
    useState<VaultLimits>(defaultVaultLimits);
  const [vaultBackendSignatory, setVaultBackendSignatory] = useState(
    defaultVaultBackendSignatory,
  );
  const [vaultFarConfig, setVaultFarConfig] =
    useState<FarConfig>(defaultFarConfig);
  const [vaultCid, setVaultCid] = useState(DEVNET_VAULT_RULES.contract_id);
  const [vaultId, setVaultId] = useState("");
  const [vaultRulesCid, setVaultRulesCid] = useState(defaultVaultRulesCid);
  const [vaultProcessorRulesCid, setVaultProcessorRulesCid] = useState(
    DEVNET_VAULT_PROCESSOR_RULES.contract_id,
  );

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
  const [allocationFactoryCid, setAllocationFactoryCid] = useState("");
  const [initialSupportedVaults, setInitialSupportedVaults] = useState<
    string[]
  >([]);
  const [farBeneficiaries, setFarBeneficiaries] = useState<
    AppRewardBeneficiary[]
  >([]);

  // Utility onboarding fields
  const [holderServiceRequestCid, setHolderServiceRequestCid] = useState("");
  const [holderParty, setHolderParty] = useState("");
  const [registrarServiceCid, setRegistrarServiceCid] = useState("");

  // Credential fields
  const [credentialId, setCredentialId] = useState("");
  const [credentialDescription, setCredentialDescription] = useState("");
  const [credentialOfferCid, setCredentialOfferCid] = useState("");
  const [claims, setClaims] = useState<Claim[]>([]);

  // Available vaults from ACS
  const [availableVaults, setAvailableVaults] = useState<VaultInfo[]>([]);
  const [vaultsLoading, setVaultsLoading] = useState(false);

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
  const [vaultRulesContracts, setVaultRulesContracts] = useState<ContractWithBlob[]>([]);
  const [allocationFactoryContracts, setAllocationFactoryContracts] = useState<ContractWithBlob[]>([]);
  const [featuredAppRightContracts, setFeaturedAppRightContracts] = useState<ContractWithBlob[]>([]);
  const [amuletRulesLoading, setAmuletRulesLoading] = useState(false);
  const [deployContractsLoading, setDeployContractsLoading] = useState(false);

  // Burn Mint Factory (from external API, used for processor deployment)
  const [burnMintFactory, setBurnMintFactory] = useState<ContractWithBlob | null>(null);
  const [burnMintFactoryLoading, setBurnMintFactoryLoading] = useState(false);

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
      setGovernanceState(body.state);
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

  // Fetch available vaults from ACS
  const fetchVaults = useCallback(async () => {
    setVaultsLoading(true);
    try {
      const res = await authenticatedFetch(
        `${API_BASE}/vaults?party_id=${encodeURIComponent(partyId)}`,
      );
      if (res.ok) {
        const response: VaultsResponse = await res.json();
        setAvailableVaults(response.vaults);
        // Auto-select all vaults for processor deployment
        setInitialSupportedVaults(response.vaults.map((v) => v.contract_id));
        // Auto-select first vault for yield epoch deployment
        if (response.vaults.length > 0) {
          setVaultCid(response.vaults[0].contract_id);
        }
      }
    } catch (e) {
      console.error("Failed to fetch vaults:", e);
    } finally {
      setVaultsLoading(false);
    }
  }, [partyId]);

  // Fetch vaults when action type needs vault selection
  useEffect(() => {
    if (
      selectedActionType === "processor_deployment_request" ||
      selectedActionType === "yield_epoch_deployment" ||
      selectedActionType === "vault_pause" ||
      selectedActionType === "vault_unpause" ||
      selectedActionType === "vault_update_limits" ||
      selectedActionType === "vault_update_backend" ||
      selectedActionType === "vault_update_far_beneficiaries"
    ) {
      fetchVaults();
    }
  }, [selectedActionType, fetchVaults]);

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
      proposalType === "accept_free_credential"
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
      proposalType === "set_provider_app_reward_beneficiaries"
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
    if (proposalType === "accept_mint_request") {
      fetchOpenMintRequests();
    }
    if (proposalType === "accept_burn_request") {
      fetchOpenBurnRequests();
    }
  }, [proposalType, fetchContractsByTemplate, fetchOpenMintRequests, fetchOpenBurnRequests]);

  // Fetch all deployment-related contracts for vault_deployment
  const fetchDeployContracts = useCallback(async () => {
    setDeployContractsLoading(true);
    try {
      const [vaultRules, allocFactory, featAppRight, registrar] = await Promise.all([
        fetchContractsByTemplate(TEMPLATE_VAULT_RULES),
        fetchContractsByTemplate(TEMPLATE_ALLOCATION_FACTORY),
        fetchContractsByTemplate(INTERFACE_FEATURED_APP_RIGHT),
        fetchContractsByTemplate(TEMPLATE_REGISTRAR_SERVICE),
      ]);
      setVaultRulesContracts(vaultRules);
      if (vaultRules.length > 0) setVaultRulesCid(vaultRules[0].contract_id);
      setAllocationFactoryContracts(allocFactory);
      if (allocFactory.length > 0) setAllocationFactoryCid(allocFactory[0].contract_id);
      setFeaturedAppRightContracts(featAppRight);
      if (featAppRight.length > 0) {
        setVaultFarConfig((prev) => ({
          ...prev,
          featured_app_right_cid: featAppRight[0].contract_id,
        }));
      }
      setRegistrarServiceContracts(registrar);
      if (registrar.length > 0) setRegistrarServiceCid(registrar[0].contract_id);
    } catch (e) {
      console.error("Failed to fetch deployment contracts:", e);
    } finally {
      setDeployContractsLoading(false);
    }
  }, [fetchContractsByTemplate]);

  // Fetch burn mint factory from external API (for processor deployment)
  const fetchBurnMintFactory = useCallback(async () => {
    setBurnMintFactoryLoading(true);
    try {
      const res = await authenticatedFetch(`${API_BASE}/token-standard-contracts`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ chain: "canton-devnet" }),
      });
      if (res.ok) {
        const data = await res.json();
        if (data.burn_mint_factory) {
          setBurnMintFactory({
            contract_id: data.burn_mint_factory.contract_id,
            blob: data.burn_mint_factory.created_event_blob,
          });
        }
      }
    } catch (e) {
      console.error("Failed to fetch burn mint factory:", e);
    } finally {
      setBurnMintFactoryLoading(false);
    }
  }, []);

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
      }
    } catch (e) {
      console.error("Failed to fetch network info:", e);
    } finally {
      setAmuletRulesLoading(false);
    }
  }, []);

  // Fetch deployment contracts when action type needs contract CIDs
  useEffect(() => {
    if (
      selectedActionType === "vault_deployment" ||
      selectedActionType === "processor_deployment_request" ||
      selectedActionType === "yield_epoch_deployment"
    ) {
      fetchDeployContracts();
    }
    if (selectedActionType === "processor_deployment_request") {
      fetchBurnMintFactory();
    }
    if (selectedActionType === "dev_net_feature_app") {
      fetchNetworkInfo();
    }
  }, [selectedActionType, fetchDeployContracts, fetchBurnMintFactory, fetchNetworkInfo]);

  // Setup CC Preapproval needs the DSO party id from the network-info
  // endpoint to prefill `expected_dso`. Mirror the action-form trigger above
  // for the proposal form.
  useEffect(() => {
    if (proposalType === "setup_cc_preapproval" && !dsoPartyId) {
      fetchNetworkInfo();
    }
  }, [proposalType, dsoPartyId, fetchNetworkInfo]);

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
      case "vault_deployment":
        return {
          type: "vault_deployment",
          vault_rules_cid: vaultRulesCid,
          vault_name: vaultName,
          share_symbol: shareSymbol,
          asset_instrument_id: assetInstrumentId,
          limits: vaultLimits,
          vault_backend_signatory: vaultBackendSignatory,
          vault_far_config:
            vaultFarConfig.featured_app_right_cid ||
            vaultFarConfig.beneficiaries.length > 0
              ? vaultFarConfig
              : undefined,
          allocation_factory_cid: allocationFactoryCid,
          registrar_service_cid: registrarServiceCid,
        };
      case "yield_epoch_deployment":
        return {
          type: "yield_epoch_deployment",
          vault_rules_cid: vaultRulesCid,
          vault_cid: vaultCid,
          asset_instrument_id: assetInstrumentId,
          vault_backend_signatory: vaultBackendSignatory,
        };
      case "vault_pause":
        return { type: "vault_pause", vault_id: vaultId };
      case "vault_unpause":
        return { type: "vault_unpause", vault_id: vaultId };
      case "vault_update_limits":
        return {
          type: "vault_update_limits",
          vault_id: vaultId,
          new_limits: vaultLimits,
        };
      case "vault_update_backend":
        return {
          type: "vault_update_backend",
          vault_id: vaultId,
          new_backend_signatory: vaultBackendSignatory,
        };
      case "vault_update_far_beneficiaries":
        return {
          type: "vault_update_far_beneficiaries",
          vault_id: vaultId,
          new_beneficiaries: farBeneficiaries,
        };
      case "processor_deployment_request":
        return {
          type: "processor_deployment_request",
          vault_processor_rules_cid: vaultProcessorRulesCid,
          vault_backend_signatory: vaultBackendSignatory,
          allocation_factory_cid: burnMintFactory?.contract_id || "",
          processor_far_config:
            vaultFarConfig.featured_app_right_cid ||
            vaultFarConfig.beneficiaries.length > 0
              ? vaultFarConfig
              : undefined,
          initial_supported_vaults: initialSupportedVaults,
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

  const validateBeneficiaryWeights = (
    beneficiaries: AppRewardBeneficiary[],
  ): string | null => {
    if (beneficiaries.length === 0) return null;
    const sum = beneficiaries.reduce(
      (acc, b) => acc + (parseFloat(b.weight) || 0),
      0,
    );
    if (Math.abs(sum - 1.0) > 1e-9) {
      return `FAR beneficiary weights must sum to exactly 1.0, got ${sum}`;
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
    setVaultName(defaultVaultName);
    setShareSymbol(defaultShareSymbol);
    setAssetInstrumentId(defaultInstrumentId);
    setVaultLimits(defaultVaultLimits);
    setVaultBackendSignatory(defaultVaultBackendSignatory);
    setVaultFarConfig(defaultFarConfig);
    setVaultCid(DEVNET_VAULT_RULES.contract_id);
    setVaultId("");
    setVaultRulesCid(defaultVaultRulesCid);
    setVaultProcessorRulesCid(DEVNET_VAULT_PROCESSOR_RULES.contract_id);
    setProviderServiceCid("");
    setUserServiceCid("");
    setAllocationFactoryCid("");
    setInitialSupportedVaults([]);
    setFarBeneficiaries([]);
    setHolderServiceRequestCid("");
    setHolderParty("");
    setRegistrarServiceCid("");
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

    // Validate beneficiary weights
    let weightsError: string | null = null;
    if (
      action.type === "vault_deployment" &&
      action.vault_far_config
    ) {
      weightsError = validateBeneficiaryWeights(
        action.vault_far_config.beneficiaries,
      );
    } else if (
      action.type === "processor_deployment_request" &&
      action.processor_far_config
    ) {
      weightsError = validateBeneficiaryWeights(
        action.processor_far_config.beneficiaries,
      );
    } else if (action.type === "vault_update_far_beneficiaries") {
      weightsError = validateBeneficiaryWeights(action.new_beneficiaries);
    }
    if (weightsError) {
      setError(weightsError);
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
    setProposalRegistrarServiceCid("");
    setProposalEnableResultContracts("true");
    setProposalAllocationFactoryCid("");
    setProposalRecipient("");
    setProposalHolder("");
    setProposalUserServiceCid("");
    setProposalCredentialId("");
    setProposalCredentialClaimsText("");
    setProposalCredentialOfferCid("");
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
        case "transfer":
          proposal = {
            type: "transfer",
            transfer_factory_cid: proposalTransferFactoryCid,
            expected_admin: proposalExpectedAdmin,
            receiver: proposalReceiver,
            amount: proposalAmount,
            instrument_id: { admin: proposalInstrumentIdAdmin, id: proposalInstrumentIdId },
            input_holding_cids: proposalInputHoldingCids ? proposalInputHoldingCids.split(",").map((s) => s.trim()).filter(Boolean) : [],
          };
          break;
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
            provider_app_reward_beneficiaries: beneficiaries,
          };
          break;
        }
        case "set_enable_result_contracts":
          proposal = {
            type: "set_enable_result_contracts",
            registrar_service_cid: proposalRegistrarServiceCid,
            enable_result_contracts:
              proposalEnableResultContracts === "clear"
                ? null
                : proposalEnableResultContracts === "true",
          };
          break;
        case "create_delegated_batched_markers_proxy":
          proposal = {
            type: "create_delegated_batched_markers_proxy",
            operator: proposalOperator,
          };
          break;
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
            description: proposalDescription,
          };
          break;
        case "accept_burn_request":
          proposal = {
            type: "accept_burn_request",
            burn_request_cid: proposalBurnRequestCid,
            instrument_configuration_cid: proposalInstrumentConfigurationCid,
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
      case "vault_pause":
      case "vault_unpause":
        return (
          <FormControl fullWidth size="small">
            <InputLabel>
              <TextHelp
                text={
                  selectedActionType === "vault_pause"
                    ? "Which vault to pause. While paused, deposits and withdrawals are blocked."
                    : "Which vault to unpause so deposits and withdrawals can resume."
                }
              >
                Vault
              </TextHelp>
            </InputLabel>
            <Select
              value={vaultId}
              label="Vault"
              onChange={(e) => setVaultId(e.target.value)}
              MenuProps={{ disableScrollLock: true }}
            >
              {vaultsLoading ? (
                <MenuItem disabled>Loading vaults...</MenuItem>
              ) : availableVaults.length > 0 ? (
                availableVaults.map((vault) => (
                  <MenuItem key={vault.contract_id} value={vault.contract_id}>
                    {vault.vault_name} ({vault.share_symbol})
                    {vault.is_paused ? " [Paused]" : ""}
                  </MenuItem>
                ))
              ) : (
                <MenuItem disabled>No vaults found</MenuItem>
              )}
            </Select>
          </FormControl>
        );
      case "vault_update_limits":
        return (
          <>
            <FormControl fullWidth size="small" sx={{ mb: 2 }}>
              <InputLabel>
                <TextHelp text="The vault whose deposit and withdrawal limits will be updated.">
                  Vault
                </TextHelp>
              </InputLabel>
              <Select
                value={vaultId}
                label="Vault"
                onChange={(e) => setVaultId(e.target.value)}
                MenuProps={{ disableScrollLock: true }}
              >
                {vaultsLoading ? (
                  <MenuItem disabled>Loading vaults...</MenuItem>
                ) : availableVaults.length > 0 ? (
                  availableVaults.map((vault) => (
                    <MenuItem key={vault.contract_id} value={vault.contract_id}>
                      {vault.vault_name} ({vault.share_symbol})
                      {vault.is_paused ? " [Paused]" : ""}
                    </MenuItem>
                  ))
                ) : (
                  <MenuItem disabled>No vaults found</MenuItem>
                )}
              </Select>
            </FormControl>
            <Typography variant="caption" color="text.secondary">
              Vault Limits (Optional - leave empty for no limit)
            </Typography>
            <TextField
              label="Max Total Deposit"
              value={vaultLimits.max_total_deposit || ""}
              onChange={(e) =>
                setVaultLimits({
                  ...vaultLimits,
                  max_total_deposit: e.target.value || undefined,
                })
              }
              size="small"
              fullWidth
              sx={{ mb: 2 }}
              placeholder="Leave empty for no limit"
              slotProps={{
                input: {
                  endAdornment: fieldHelpAdornment(
                    "Cap on the total asset amount this vault can hold across all depositors. Leave empty for no cap.",
                    "Help for Max Total Deposit",
                  ),
                },
              }}
            />
            <TextField
              label="Min Deposit Amount"
              value={vaultLimits.min_deposit_amount || ""}
              onChange={(e) =>
                setVaultLimits({
                  ...vaultLimits,
                  min_deposit_amount: e.target.value || undefined,
                })
              }
              size="small"
              fullWidth
              sx={{ mb: 2 }}
              placeholder="Leave empty for no limit"
              slotProps={{
                input: {
                  endAdornment: fieldHelpAdornment(
                    "Smallest single deposit this vault will accept. Leave empty for no minimum.",
                    "Help for Min Deposit Amount",
                  ),
                },
              }}
            />
            <TextField
              label="Min Withdrawal Amount"
              value={vaultLimits.min_withdrawal_amount || ""}
              onChange={(e) =>
                setVaultLimits({
                  ...vaultLimits,
                  min_withdrawal_amount: e.target.value || undefined,
                })
              }
              size="small"
              fullWidth
              placeholder="Leave empty for no limit"
              slotProps={{
                input: {
                  endAdornment: fieldHelpAdornment(
                    "Smallest single withdrawal this vault will accept. Leave empty for no minimum.",
                    "Help for Min Withdrawal Amount",
                  ),
                },
              }}
            />
          </>
        );
      case "vault_update_backend":
        return (
          <>
            <FormControl fullWidth size="small" sx={{ mb: 2 }}>
              <InputLabel>
                <TextHelp text="The vault whose backend signatory will be updated.">
                  Vault
                </TextHelp>
              </InputLabel>
              <Select
                value={vaultId}
                label="Vault"
                onChange={(e) => setVaultId(e.target.value)}
                MenuProps={{ disableScrollLock: true }}
              >
                {vaultsLoading ? (
                  <MenuItem disabled>Loading vaults...</MenuItem>
                ) : availableVaults.length > 0 ? (
                  availableVaults.map((vault) => (
                    <MenuItem key={vault.contract_id} value={vault.contract_id}>
                      {vault.vault_name} ({vault.share_symbol})
                      {vault.is_paused ? " [Paused]" : ""}
                    </MenuItem>
                  ))
                ) : (
                  <MenuItem disabled>No vaults found</MenuItem>
                )}
              </Select>
            </FormControl>
            <TextField
              label="New Backend Signatory"
              value={vaultBackendSignatory}
              onChange={(e) => setVaultBackendSignatory(e.target.value)}
              size="small"
              fullWidth
              slotProps={{
                input: {
                  endAdornment: fieldHelpAdornment(
                    "Party id of the backend service that co-signs vault deposits and withdrawals.",
                    "Help for New Backend Signatory",
                  ),
                },
              }}
            />
          </>
        );
      case "vault_deployment":
        return (
          <>
            <Box sx={{ display: "flex", gap: 1, alignItems: "center", mb: 2 }}>
              <FormControl fullWidth size="small" required>
                <InputLabel>
                  <TextHelp text="VaultRules template contract that defines how this vault behaves. Pick from the deployed DAR's available rules contracts.">
                    Vault Rules
                  </TextHelp>
                </InputLabel>
                <Select
                  value={vaultRulesCid}
                  label="Vault Rules"
                  onChange={(e) => setVaultRulesCid(e.target.value)}
                  MenuProps={{ disableScrollLock: true }}
                >
                  {deployContractsLoading ? (
                    <MenuItem disabled>Loading...</MenuItem>
                  ) : vaultRulesContracts.length > 0 ? (
                    vaultRulesContracts.map((c) => (
                      <MenuItem key={c.contract_id} value={c.contract_id}>
                        {c.contract_id}
                      </MenuItem>
                    ))
                  ) : (
                    <MenuItem disabled>No vault rules found</MenuItem>
                  )}
                </Select>
              </FormControl>
              <Tooltip title="Refresh">
                <span>
                  <IconButton
                    size="small"
                    onClick={fetchDeployContracts}
                    disabled={deployContractsLoading}
                  >
                    {deployContractsLoading ? <CircularProgress size={20} /> : <RefreshIcon />}
                  </IconButton>
                </span>
              </Tooltip>
            </Box>
            <TextField
              label="Vault Name"
              value={vaultName}
              onChange={(e) => setVaultName(e.target.value)}
              size="small"
              fullWidth
              sx={{ mb: 2 }}
              slotProps={{
                input: {
                  endAdornment: fieldHelpAdornment(
                    "Human-readable name for the new vault, shown in the UI.",
                    "Help for Vault Name",
                  ),
                },
              }}
            />
            <TextField
              label="Share Symbol"
              value={shareSymbol}
              onChange={(e) => setShareSymbol(e.target.value)}
              size="small"
              fullWidth
              sx={{ mb: 2 }}
              slotProps={{
                input: {
                  endAdornment: fieldHelpAdornment(
                    "Ticker symbol for vault shares (e.g. CBTCV0).",
                    "Help for Share Symbol",
                  ),
                },
              }}
            />
            <Typography variant="caption" color="text.secondary">
              Asset Instrument ID
            </Typography>
            <TextField
              label="Admin Party"
              value={assetInstrumentId.admin}
              onChange={(e) =>
                setAssetInstrumentId({
                  ...assetInstrumentId,
                  admin: e.target.value,
                })
              }
              size="small"
              fullWidth
              sx={{ mb: 1 }}
              slotProps={{
                input: {
                  endAdornment: fieldHelpAdornment(
                    "Issuer party of the underlying asset this vault holds (e.g. the cBTC dec-party).",
                    "Help for Admin Party",
                  ),
                },
              }}
            />
            <TextField
              label="ID"
              value={assetInstrumentId.id}
              onChange={(e) =>
                setAssetInstrumentId({
                  ...assetInstrumentId,
                  id: e.target.value,
                })
              }
              size="small"
              fullWidth
              sx={{ mb: 2 }}
              slotProps={{
                input: {
                  endAdornment: fieldHelpAdornment(
                    "Token name of the underlying asset (e.g. \"CBTC\"). Combined with the admin party, identifies the instrument.",
                    "Help for ID",
                  ),
                },
              }}
            />
            <Typography variant="caption" color="text.secondary">
              Vault Limits (Optional)
            </Typography>
            <TextField
              label="Max Total Deposit"
              value={vaultLimits.max_total_deposit || ""}
              onChange={(e) =>
                setVaultLimits({
                  ...vaultLimits,
                  max_total_deposit: e.target.value || undefined,
                })
              }
              size="small"
              fullWidth
              sx={{ mb: 1 }}
              placeholder="Leave empty for no limit"
              slotProps={{
                input: {
                  endAdornment: fieldHelpAdornment(
                    "Cap on the total asset amount this vault can hold. Leave empty for no cap.",
                    "Help for Max Total Deposit",
                  ),
                },
              }}
            />
            <TextField
              label="Min Deposit Amount"
              value={vaultLimits.min_deposit_amount || ""}
              onChange={(e) =>
                setVaultLimits({
                  ...vaultLimits,
                  min_deposit_amount: e.target.value || undefined,
                })
              }
              size="small"
              fullWidth
              sx={{ mb: 1 }}
              placeholder="Leave empty for no limit"
              slotProps={{
                input: {
                  endAdornment: fieldHelpAdornment(
                    "Smallest single deposit this vault will accept. Leave empty for no minimum.",
                    "Help for Min Deposit Amount",
                  ),
                },
              }}
            />
            <TextField
              label="Min Withdrawal Amount"
              value={vaultLimits.min_withdrawal_amount || ""}
              onChange={(e) =>
                setVaultLimits({
                  ...vaultLimits,
                  min_withdrawal_amount: e.target.value || undefined,
                })
              }
              size="small"
              fullWidth
              sx={{ mb: 2 }}
              placeholder="Leave empty for no limit"
              slotProps={{
                input: {
                  endAdornment: fieldHelpAdornment(
                    "Smallest single withdrawal this vault will accept. Leave empty for no minimum.",
                    "Help for Min Withdrawal Amount",
                  ),
                },
              }}
            />
            <TextField
              label="Vault Backend Signatory Party"
              value={vaultBackendSignatory}
              onChange={(e) => setVaultBackendSignatory(e.target.value)}
              size="small"
              fullWidth
              sx={{ mb: 2 }}
              slotProps={{
                input: {
                  endAdornment: fieldHelpAdornment(
                    "Party id of the backend service that co-signs vault deposits and withdrawals.",
                    "Help for Vault Backend Signatory Party",
                  ),
                },
              }}
            />
            <Typography variant="caption" color="text.secondary" sx={{ display: "block" }}>
              FAR Config (Optional)
            </Typography>
            <Box sx={{ display: "flex", gap: 1, alignItems: "center", mb: 1 }}>
              <FormControl fullWidth size="small">
                <InputLabel>
                  <TextHelp text="FeaturedAppRight contract that lets this vault collect app rewards from Canton Coin.">
                    Featured App Right
                  </TextHelp>
                </InputLabel>
                <Select
                  value={vaultFarConfig.featured_app_right_cid}
                  label="Featured App Right"
                  onChange={(e) =>
                    setVaultFarConfig({
                      ...vaultFarConfig,
                      featured_app_right_cid: e.target.value,
                    })
                  }
                  MenuProps={{ disableScrollLock: true }}
                >
                  {deployContractsLoading ? (
                    <MenuItem disabled>Loading...</MenuItem>
                  ) : featuredAppRightContracts.length > 0 ? (
                    featuredAppRightContracts.map((c) => (
                      <MenuItem key={c.contract_id} value={c.contract_id}>
                        {c.contract_id}
                      </MenuItem>
                    ))
                  ) : (
                    <MenuItem disabled>No featured app rights found</MenuItem>
                  )}
                </Select>
              </FormControl>
              <Tooltip title="Refresh">
                <span>
                  <IconButton
                    size="small"
                    onClick={fetchDeployContracts}
                    disabled={deployContractsLoading}
                  >
                    {deployContractsLoading ? <CircularProgress size={20} /> : <RefreshIcon />}
                  </IconButton>
                </span>
              </Tooltip>
            </Box>
            <Box sx={{ mb: 2 }}>
              <Typography variant="caption" color="text.secondary" sx={{ display: "block", mb: 1 }}>
                <TextHelp text="Parties that share app rewards earned by this vault. Each row is a party plus a weight; weights are decimals and must sum to 1.0.">
                  FAR Beneficiaries (who receives app rewards)
                </TextHelp>
              </Typography>
              {vaultFarConfig.beneficiaries.map((b, idx) => (
                <Box key={idx} sx={{ display: "flex", gap: 1, mb: 1 }}>
                  <TextField
                    label="Beneficiary Party"
                    value={b.beneficiary}
                    onChange={(e) => {
                      const updated = [...vaultFarConfig.beneficiaries];
                      updated[idx] = { ...b, beneficiary: e.target.value };
                      setVaultFarConfig({
                        ...vaultFarConfig,
                        beneficiaries: updated,
                      });
                    }}
                    size="small"
                    sx={{ flex: 2 }}
                    slotProps={{
                      input: {
                        endAdornment: fieldHelpAdornment(
                          "Party id that receives this share of the vault's app rewards.",
                          "Help for Beneficiary Party",
                        ),
                      },
                    }}
                  />
                  <TextField
                    label="Weight"
                    value={b.weight}
                    onChange={(e) => {
                      const updated = [...vaultFarConfig.beneficiaries];
                      updated[idx] = { ...b, weight: e.target.value };
                      setVaultFarConfig({
                        ...vaultFarConfig,
                        beneficiaries: updated,
                      });
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
                      setVaultFarConfig({
                        ...vaultFarConfig,
                        beneficiaries: vaultFarConfig.beneficiaries.filter(
                          (_, i) => i !== idx,
                        ),
                      })
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
                    setVaultFarConfig({
                      ...vaultFarConfig,
                      beneficiaries: [
                        ...vaultFarConfig.beneficiaries,
                        { beneficiary: "", weight: "1.0" },
                      ],
                    })
                  }
                >
                  Add Beneficiary
                </Button>
                {vaultFarConfig.beneficiaries.length > 0 && (() => {
                  const sum = vaultFarConfig.beneficiaries.reduce(
                    (acc, b) => acc + (parseFloat(b.weight) || 0), 0,
                  );
                  const isValid = Math.abs(sum - 1.0) < 1e-9;
                  return (
                    <Typography
                      variant="caption"
                      color={isValid ? "success.main" : "error.main"}
                    >
                      Sum: {sum.toFixed(4)} {isValid ? "" : "(must be 1.0)"}
                    </Typography>
                  );
                })()}
              </Box>
            </Box>
            <Box sx={{ display: "flex", gap: 1, alignItems: "center", mb: 2 }}>
              <FormControl fullWidth size="small" required>
                <InputLabel>
                  <TextHelp text="AllocationFactory contract used to mint vault share allocations on deposit.">
                    Allocation Factory
                  </TextHelp>
                </InputLabel>
                <Select
                  value={allocationFactoryCid}
                  label="Allocation Factory"
                  onChange={(e) => setAllocationFactoryCid(e.target.value)}
                  MenuProps={{ disableScrollLock: true }}
                >
                  {deployContractsLoading ? (
                    <MenuItem disabled>Loading...</MenuItem>
                  ) : allocationFactoryContracts.length > 0 ? (
                    allocationFactoryContracts.map((c) => (
                      <MenuItem key={c.contract_id} value={c.contract_id}>
                        {c.contract_id}
                      </MenuItem>
                    ))
                  ) : (
                    <MenuItem disabled>No allocation factories found</MenuItem>
                  )}
                </Select>
              </FormControl>
              <Tooltip title="Refresh">
                <span>
                  <IconButton
                    size="small"
                    onClick={fetchDeployContracts}
                    disabled={deployContractsLoading}
                  >
                    {deployContractsLoading ? <CircularProgress size={20} /> : <RefreshIcon />}
                  </IconButton>
                </span>
              </Tooltip>
            </Box>
            <Box sx={{ display: "flex", gap: 1, alignItems: "center" }}>
              <FormControl fullWidth size="small" required>
                <InputLabel>
                  <TextHelp text="RegistrarService contract that registers this vault's instrument with the utility.">
                    Registrar Service
                  </TextHelp>
                </InputLabel>
                <Select
                  value={registrarServiceCid}
                  label="Registrar Service"
                  onChange={(e) => setRegistrarServiceCid(e.target.value)}
                  MenuProps={{ disableScrollLock: true }}
                >
                  {deployContractsLoading ? (
                    <MenuItem disabled>Loading...</MenuItem>
                  ) : registrarServiceContracts.length > 0 ? (
                    registrarServiceContracts.map((c) => (
                      <MenuItem key={c.contract_id} value={c.contract_id}>
                        {c.contract_id}
                      </MenuItem>
                    ))
                  ) : (
                    <MenuItem disabled>No registrar services found</MenuItem>
                  )}
                </Select>
              </FormControl>
              <Tooltip title="Refresh">
                <span>
                  <IconButton
                    size="small"
                    onClick={fetchDeployContracts}
                    disabled={deployContractsLoading}
                  >
                    {deployContractsLoading ? <CircularProgress size={20} /> : <RefreshIcon />}
                  </IconButton>
                </span>
              </Tooltip>
            </Box>
          </>
        );
      case "yield_epoch_deployment":
        return (
          <>
            <Box sx={{ display: "flex", gap: 1, alignItems: "center", mb: 2 }}>
              <FormControl fullWidth size="small" required>
                <InputLabel>
                  <TextHelp text="VaultRules contract that the new yield epoch will run under.">
                    Vault Rules
                  </TextHelp>
                </InputLabel>
                <Select
                  value={vaultRulesCid}
                  label="Vault Rules"
                  onChange={(e) => setVaultRulesCid(e.target.value)}
                  MenuProps={{ disableScrollLock: true }}
                >
                  {deployContractsLoading ? (
                    <MenuItem disabled>Loading...</MenuItem>
                  ) : vaultRulesContracts.length > 0 ? (
                    vaultRulesContracts.map((c) => (
                      <MenuItem key={c.contract_id} value={c.contract_id}>
                        {c.contract_id}
                      </MenuItem>
                    ))
                  ) : (
                    <MenuItem disabled>No vault rules found</MenuItem>
                  )}
                </Select>
              </FormControl>
              <Tooltip title="Refresh">
                <span>
                  <IconButton
                    size="small"
                    onClick={fetchDeployContracts}
                    disabled={deployContractsLoading}
                  >
                    {deployContractsLoading ? <CircularProgress size={20} /> : <RefreshIcon />}
                  </IconButton>
                </span>
              </Tooltip>
            </Box>
            <FormControl fullWidth size="small" sx={{ mb: 2 }}>
              <InputLabel>
                <TextHelp text="Existing vault to attach this new yield epoch to.">
                  Vault
                </TextHelp>
              </InputLabel>
              <Select
                value={vaultCid}
                label="Vault"
                onChange={(e) => setVaultCid(e.target.value)}
                MenuProps={{ disableScrollLock: true }}
              >
                {vaultsLoading ? (
                  <MenuItem disabled>Loading vaults...</MenuItem>
                ) : availableVaults.length > 0 ? (
                  availableVaults.map((vault) => (
                    <MenuItem key={vault.contract_id} value={vault.contract_id}>
                      {vault.vault_name} ({vault.share_symbol})
                      {vault.is_paused ? " [Paused]" : ""}
                    </MenuItem>
                  ))
                ) : (
                  <MenuItem disabled>No vaults found</MenuItem>
                )}
              </Select>
            </FormControl>
            <Typography variant="caption" color="text.secondary">
              Asset Instrument ID
            </Typography>
            <TextField
              label="Admin Party"
              value={assetInstrumentId.admin}
              onChange={(e) =>
                setAssetInstrumentId({
                  ...assetInstrumentId,
                  admin: e.target.value,
                })
              }
              size="small"
              fullWidth
              sx={{ mb: 1 }}
              slotProps={{
                input: {
                  endAdornment: fieldHelpAdornment(
                    "Issuer party of the asset this vault holds.",
                    "Help for Admin Party",
                  ),
                },
              }}
            />
            <TextField
              label="ID"
              value={assetInstrumentId.id}
              onChange={(e) =>
                setAssetInstrumentId({
                  ...assetInstrumentId,
                  id: e.target.value,
                })
              }
              size="small"
              fullWidth
              sx={{ mb: 2 }}
              slotProps={{
                input: {
                  endAdornment: fieldHelpAdornment(
                    "Token name of the underlying asset (e.g. \"CBTC\").",
                    "Help for ID",
                  ),
                },
              }}
            />
            <TextField
              label="Vault Backend Signatory Party"
              value={vaultBackendSignatory}
              onChange={(e) => setVaultBackendSignatory(e.target.value)}
              size="small"
              fullWidth
              slotProps={{
                input: {
                  endAdornment: fieldHelpAdornment(
                    "Party id of the backend service that co-signs yield epoch settlement.",
                    "Help for Vault Backend Signatory Party",
                  ),
                },
              }}
            />
          </>
        );
      case "vault_update_far_beneficiaries":
        return (
          <>
            <FormControl fullWidth size="small" sx={{ mb: 2 }}>
              <InputLabel>
                <TextHelp text="The vault whose app-reward beneficiaries you want to update.">
                  Vault
                </TextHelp>
              </InputLabel>
              <Select
                value={vaultId}
                label="Vault"
                onChange={(e) => setVaultId(e.target.value)}
                MenuProps={{ disableScrollLock: true }}
              >
                {vaultsLoading ? (
                  <MenuItem disabled>Loading vaults...</MenuItem>
                ) : availableVaults.length > 0 ? (
                  availableVaults.map((vault) => (
                    <MenuItem key={vault.contract_id} value={vault.contract_id}>
                      {vault.vault_name} ({vault.share_symbol})
                      {vault.is_paused ? " [Paused]" : ""}
                    </MenuItem>
                  ))
                ) : (
                  <MenuItem disabled>No vaults found</MenuItem>
                )}
              </Select>
            </FormControl>
            <Typography variant="caption" color="text.secondary" sx={{ display: "block" }}>
              <TextHelp text="Parties that share this vault's app rewards. Each row is a party plus a weight; weights are decimals and must sum to 1.0.">
                FAR Beneficiaries (add beneficiary party + weight)
              </TextHelp>
            </Typography>
            {farBeneficiaries.map((b, idx) => (
              <Box key={idx} sx={{ display: "flex", gap: 1, mb: 1 }}>
                <TextField
                  label="Beneficiary Party"
                  value={b.beneficiary}
                  onChange={(e) => {
                    const updated = [...farBeneficiaries];
                    updated[idx] = { ...b, beneficiary: e.target.value };
                    setFarBeneficiaries(updated);
                  }}
                  size="small"
                  sx={{ flex: 2 }}
                  slotProps={{
                    input: {
                      endAdornment: fieldHelpAdornment(
                        "Party id that receives this share of the vault's app rewards.",
                        "Help for Beneficiary Party",
                      ),
                    },
                  }}
                />
                <TextField
                  label="Weight"
                  value={b.weight}
                  onChange={(e) => {
                    const updated = [...farBeneficiaries];
                    updated[idx] = { ...b, weight: e.target.value };
                    setFarBeneficiaries(updated);
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
                    setFarBeneficiaries(
                      farBeneficiaries.filter((_, i) => i !== idx),
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
                  setFarBeneficiaries([
                    ...farBeneficiaries,
                    { beneficiary: "", weight: "1" },
                  ])
                }
              >
                Add Beneficiary
              </Button>
              {farBeneficiaries.length > 0 && (() => {
                const sum = farBeneficiaries.reduce(
                  (acc, b) => acc + (parseFloat(b.weight) || 0), 0,
                );
                const isValid = Math.abs(sum - 1.0) < 1e-9;
                return (
                  <Typography
                    variant="caption"
                    color={isValid ? "success.main" : "error.main"}
                  >
                    Sum: {sum.toFixed(4)} {isValid ? "" : "(must be 1.0)"}
                  </Typography>
                );
              })()}
            </Box>
          </>
        );
      case "processor_deployment_request":
        return (
          <>
            <TextField
              label="Vault Processor Rules Contract ID"
              value={vaultProcessorRulesCid}
              onChange={(e) => setVaultProcessorRulesCid(e.target.value)}
              size="small"
              fullWidth
              sx={{ mb: 2 }}
              required
              slotProps={{
                input: {
                  endAdornment: fieldHelpAdornment(
                    "Contract id of the VaultProcessorRules template that defines processor behaviour.",
                    "Help for Vault Processor Rules Contract ID",
                  ),
                },
              }}
            />
            <TextField
              label="Vault Backend Signatory Party"
              value={vaultBackendSignatory}
              onChange={(e) => setVaultBackendSignatory(e.target.value)}
              size="small"
              fullWidth
              sx={{ mb: 2 }}
              slotProps={{
                input: {
                  endAdornment: fieldHelpAdornment(
                    "Party id of the backend service that co-signs processor actions.",
                    "Help for Vault Backend Signatory Party",
                  ),
                },
              }}
            />
            <Box sx={{ display: "flex", gap: 1, alignItems: "center", mb: 2 }}>
              <TextField
                label="Burn Mint Factory"
                value={burnMintFactory?.contract_id || ""}
                onChange={(e) =>
                  setBurnMintFactory(
                    e.target.value
                      ? { contract_id: e.target.value, blob: burnMintFactory?.blob || "" }
                      : null,
                  )
                }
                size="small"
                fullWidth
                required
                helperText={
                  burnMintFactoryLoading
                    ? "Loading..."
                    : !burnMintFactory
                      ? "Not available"
                      : undefined
                }
                slotProps={{
                  input: {
                    endAdornment: fieldHelpAdornment(
                      "BurnMintFactory contract used by the processor to mint and burn vault shares.",
                      "Help for Burn Mint Factory",
                    ),
                  },
                }}
              />
              <Tooltip title="Refresh">
                <span>
                  <IconButton
                    size="small"
                    onClick={fetchBurnMintFactory}
                    disabled={burnMintFactoryLoading}
                  >
                    {burnMintFactoryLoading ? <CircularProgress size={20} /> : <RefreshIcon />}
                  </IconButton>
                </span>
              </Tooltip>
            </Box>
            <Typography variant="caption" color="text.secondary" sx={{ display: "block" }}>
              FAR Config (Optional)
            </Typography>
            <Box sx={{ display: "flex", gap: 1, alignItems: "center", mb: 1 }}>
              <FormControl fullWidth size="small">
                <InputLabel>
                  <TextHelp text="FeaturedAppRight contract that lets the processor collect app rewards.">
                    Featured App Right
                  </TextHelp>
                </InputLabel>
                <Select
                  value={vaultFarConfig.featured_app_right_cid}
                  label="Featured App Right"
                  onChange={(e) =>
                    setVaultFarConfig({
                      ...vaultFarConfig,
                      featured_app_right_cid: e.target.value,
                    })
                  }
                  MenuProps={{ disableScrollLock: true }}
                >
                  {deployContractsLoading ? (
                    <MenuItem disabled>Loading...</MenuItem>
                  ) : featuredAppRightContracts.length > 0 ? (
                    featuredAppRightContracts.map((c) => (
                      <MenuItem key={c.contract_id} value={c.contract_id}>
                        {c.contract_id}
                      </MenuItem>
                    ))
                  ) : (
                    <MenuItem disabled>No featured app rights found</MenuItem>
                  )}
                </Select>
              </FormControl>
              <Tooltip title="Refresh">
                <span>
                  <IconButton
                    size="small"
                    onClick={fetchDeployContracts}
                    disabled={deployContractsLoading}
                  >
                    {deployContractsLoading ? <CircularProgress size={20} /> : <RefreshIcon />}
                  </IconButton>
                </span>
              </Tooltip>
            </Box>
            <Box sx={{ mb: 2 }}>
              <Typography variant="caption" color="text.secondary" sx={{ display: "block", mb: 1 }}>
                <TextHelp text="Parties that share the processor's app rewards. Each row is a party plus a weight; weights are decimals and must sum to 1.0.">
                  FAR Beneficiaries
                </TextHelp>
              </Typography>
              {vaultFarConfig.beneficiaries.map((b, idx) => (
                <Box key={idx} sx={{ display: "flex", gap: 1, mb: 1 }}>
                  <TextField
                    label="Beneficiary Party"
                    value={b.beneficiary}
                    onChange={(e) => {
                      const updated = [...vaultFarConfig.beneficiaries];
                      updated[idx] = { ...b, beneficiary: e.target.value };
                      setVaultFarConfig({
                        ...vaultFarConfig,
                        beneficiaries: updated,
                      });
                    }}
                    size="small"
                    sx={{ flex: 2 }}
                    slotProps={{
                      input: {
                        endAdornment: fieldHelpAdornment(
                          "Party id that receives this share of the processor's app rewards.",
                          "Help for Beneficiary Party",
                        ),
                      },
                    }}
                  />
                  <TextField
                    label="Weight"
                    value={b.weight}
                    onChange={(e) => {
                      const updated = [...vaultFarConfig.beneficiaries];
                      updated[idx] = { ...b, weight: e.target.value };
                      setVaultFarConfig({
                        ...vaultFarConfig,
                        beneficiaries: updated,
                      });
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
                      setVaultFarConfig({
                        ...vaultFarConfig,
                        beneficiaries: vaultFarConfig.beneficiaries.filter(
                          (_, i) => i !== idx,
                        ),
                      })
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
                    setVaultFarConfig({
                      ...vaultFarConfig,
                      beneficiaries: [
                        ...vaultFarConfig.beneficiaries,
                        { beneficiary: "", weight: "1.0" },
                      ],
                    })
                  }
                >
                  Add Beneficiary
                </Button>
                {vaultFarConfig.beneficiaries.length > 0 && (() => {
                  const sum = vaultFarConfig.beneficiaries.reduce(
                    (acc, b) => acc + (parseFloat(b.weight) || 0), 0,
                  );
                  const isValid = Math.abs(sum - 1.0) < 1e-9;
                  return (
                    <Typography
                      variant="caption"
                      color={isValid ? "success.main" : "error.main"}
                    >
                      Sum: {sum.toFixed(4)} {isValid ? "" : "(must be 1.0)"}
                    </Typography>
                  );
                })()}
              </Box>
            </Box>
            <Typography variant="caption" color="text.secondary" sx={{ display: "block" }}>
              <TextHelp text="Vaults that this processor will be wired up to manage from the start. You can add more vaults later via governance.">
                Initial Supported Vaults
              </TextHelp>
            </Typography>
            {vaultsLoading ? (
              <Box
                sx={{ display: "flex", alignItems: "center", gap: 1, my: 1 }}
              >
                <CircularProgress size={16} />
                <Typography variant="body2">Loading vaults...</Typography>
              </Box>
            ) : availableVaults.length > 0 ? (
              <FormGroup sx={{ ml: 1 }}>
                {availableVaults.map((vault) => (
                  <FormControlLabel
                    key={vault.contract_id}
                    control={
                      <Checkbox
                        size="small"
                        checked={initialSupportedVaults.includes(
                          vault.contract_id,
                        )}
                        onChange={(e) => {
                          if (e.target.checked) {
                            setInitialSupportedVaults([
                              ...initialSupportedVaults,
                              vault.contract_id,
                            ]);
                          } else {
                            setInitialSupportedVaults(
                              initialSupportedVaults.filter(
                                (id) => id !== vault.contract_id,
                              ),
                            );
                          }
                        }}
                      />
                    }
                    label={
                      <Box>
                        <Typography variant="body2">
                          {vault.vault_name} ({vault.share_symbol})
                          {vault.is_paused && (
                            <Chip
                              size="small"
                              label="Paused"
                              color="warning"
                              sx={{ ml: 1 }}
                            />
                          )}
                        </Typography>
                        <Typography
                          variant="caption"
                          color="text.secondary"
                          sx={{
                            fontFamily: "monospace",
                            wordBreak: "break-all",
                          }}
                        >
                          {vault.contract_id}
                        </Typography>
                      </Box>
                    }
                  />
                ))}
              </FormGroup>
            ) : (
              <Typography variant="body2" color="text.secondary" sx={{ my: 1 }}>
                No vaults found. Deploy a vault first.
              </Typography>
            )}
          </>
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
                          {label} — balance {h.amount}
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
                            label={`Available balance: ${holding.amount}`}
                            color="primary"
                          />
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
                      return holding ? n > Number(holding.amount) : false;
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
                      if (holding && n > Number(holding.amount)) {
                        return `Exceeds available balance (${holding.amount})`;
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
