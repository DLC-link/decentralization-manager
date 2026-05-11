import { useState, useEffect, useCallback } from "react";
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
  TEMPLATE_MINT_REQUEST,
  TEMPLATE_BURN_REQUEST,
} from "../constants";
import { authenticatedFetch } from "../api";
import { getActionTypeOptions } from "../governanceFormat";
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
  ContractWithBlob,
  ContractQueryResponse,
  Network,
  NetworkInfo,
  ProposeActionRequest,
  ProposalType,
  InstrumentAllowance,
  InstrumentInfo,
  InstrumentsResponse,
  TransferPreapprovalsResponse,
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
  /// Which half of the section to render:
  /// - "actions"   = governance-action confirmations + new-action form (default)
  /// - "proposals" = domain-proposal list + new-proposal form (core_self only)
  /// - undefined   = both (legacy, used when rendered inline on the party page)
  view?: "actions" | "proposals";
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
  view,
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
  const [proposalBeneficiariesText, setProposalBeneficiariesText] = useState("");
  const [proposalClearBeneficiaries, setProposalClearBeneficiaries] = useState(false);
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
  const [registrarServiceContracts, setRegistrarServiceContracts] = useState<ContractWithBlob[]>([]);
  const [mintRequestContracts, setMintRequestContracts] = useState<ContractWithBlob[]>([]);
  const [burnRequestContracts, setBurnRequestContracts] = useState<ContractWithBlob[]>([]);
  // InstrumentConfiguration contracts fetched from /instruments. Each one
  // represents a token the governance party can mint/burn against and exposes
  // its parsed instrument_admin + instrument_id, so we can drive a real
  // dropdown without the frontend having to decode contract blobs.
  const [availableInstruments, setAvailableInstruments] = useState<InstrumentInfo[]>([]);
  const [instrumentsLoading, setInstrumentsLoading] = useState(false);
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
  useEffect(() => {
    if (
      proposalType === "create_user_service_request" ||
      proposalType === "create_provider_service_request" ||
      proposalType === "setup_utility"
    ) {
      fetchServices();
    }
  }, [proposalType, fetchServices]);

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

  // Fetch contracts by template (returns CID + blob)
  const fetchContractsByTemplate = useCallback(
    async (template: {
      package_ref: string;
      module: string;
      entity: string;
      interface?: boolean;
    }) => {
      const params = new URLSearchParams({
        party_id: partyId,
        package_id: template.package_ref,
        module_name: template.module,
        entity_name: template.entity,
      });
      if (template.interface) params.set("interface", "true");
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
      fetchContractsByTemplate(TEMPLATE_MINT_REQUEST).then(setMintRequestContracts);
    }
    if (proposalType === "accept_burn_request") {
      fetchContractsByTemplate(TEMPLATE_BURN_REQUEST).then(setBurnRequestContracts);
    }
  }, [proposalType, fetchContractsByTemplate]);

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
    setNewThreshold(2);
    setTimeoutMicroseconds(3600000000);
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
      await fetchGovernance();
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
    setProposalBeneficiariesText("");
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
            const lines = proposalBeneficiariesText
              .split("\n")
              .map((line) => line.trim())
              .filter(Boolean);
            beneficiaries = lines.map((line, idx) => {
              const parts = line.split(",").map((s) => s.trim());
              if (parts.length !== 2 || !parts[0] || !parts[1]) {
                throw new Error(
                  `Beneficiary line ${idx + 1}: expected "<party>,<weight>", got "${line}"`,
                );
              }
              return { beneficiary: parts[0], weight: parts[1] };
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

      // Clear fields, keep the form visible. The created proposal shows up
      // in the notification queue — no separate success message needed.
      resetProposalForm();
      await fetchGovernance();
      onAfterAction?.();
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
      case "governance_remove_member":
        return (
          <>
            <TextField
              label="Member Party ID"
              value={memberParty}
              onChange={(e) => setMemberParty(e.target.value)}
              size="small"
              fullWidth
              sx={{ mb: 2 }}
            />
            <TextField
              label="New Threshold"
              type="number"
              value={newThreshold}
              onChange={(e) => setNewThreshold(parseInt(e.target.value) || 2)}
              size="small"
              fullWidth
            />
          </>
        );
      case "governance_set_threshold":
        return (
          <TextField
            label="New Threshold"
            type="number"
            value={newThreshold}
            onChange={(e) => setNewThreshold(parseInt(e.target.value) || 2)}
            size="small"
            fullWidth
          />
        );
      case "governance_set_timeout":
        return (
          <TextField
            label="Timeout (microseconds)"
            type="number"
            value={timeoutMicroseconds}
            onChange={(e) =>
              setTimeoutMicroseconds(parseInt(e.target.value) || 0)
            }
            size="small"
            fullWidth
            helperText="1 hour = 3,600,000,000 microseconds"
          />
        );
      case "vault_pause":
      case "vault_unpause":
        return (
          <FormControl fullWidth size="small">
            <InputLabel>Vault</InputLabel>
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
              <InputLabel>Vault</InputLabel>
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
            />
          </>
        );
      case "vault_update_backend":
        return (
          <>
            <FormControl fullWidth size="small" sx={{ mb: 2 }}>
              <InputLabel>Vault</InputLabel>
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
            />
          </>
        );
      case "vault_deployment":
        return (
          <>
            <Box sx={{ display: "flex", gap: 1, alignItems: "center", mb: 2 }}>
              <FormControl fullWidth size="small" required>
                <InputLabel>Vault Rules</InputLabel>
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
            />
            <TextField
              label="Share Symbol"
              value={shareSymbol}
              onChange={(e) => setShareSymbol(e.target.value)}
              size="small"
              fullWidth
              sx={{ mb: 2 }}
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
            />
            <TextField
              label="Vault Backend Signatory Party"
              value={vaultBackendSignatory}
              onChange={(e) => setVaultBackendSignatory(e.target.value)}
              size="small"
              fullWidth
              sx={{ mb: 2 }}
            />
            <Typography variant="caption" color="text.secondary" sx={{ display: "block" }}>
              FAR Config (Optional)
            </Typography>
            <Box sx={{ display: "flex", gap: 1, alignItems: "center", mb: 1 }}>
              <FormControl fullWidth size="small">
                <InputLabel>Featured App Right</InputLabel>
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
                FAR Beneficiaries (who receives app rewards)
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
                <InputLabel>Allocation Factory</InputLabel>
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
                <InputLabel>Registrar Service</InputLabel>
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
                <InputLabel>Vault Rules</InputLabel>
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
              <InputLabel>Vault</InputLabel>
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
            />
            <TextField
              label="Vault Backend Signatory Party"
              value={vaultBackendSignatory}
              onChange={(e) => setVaultBackendSignatory(e.target.value)}
              size="small"
              fullWidth
            />
          </>
        );
      case "vault_update_far_beneficiaries":
        return (
          <>
            <FormControl fullWidth size="small" sx={{ mb: 2 }}>
              <InputLabel>Vault</InputLabel>
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
              FAR Beneficiaries (add beneficiary party + weight)
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
            />
            <TextField
              label="Vault Backend Signatory Party"
              value={vaultBackendSignatory}
              onChange={(e) => setVaultBackendSignatory(e.target.value)}
              size="small"
              fullWidth
              sx={{ mb: 2 }}
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
                <InputLabel>Featured App Right</InputLabel>
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
                FAR Beneficiaries
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
              Initial Supported Vaults
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
            />
            <Box sx={{ display: "flex", gap: 1, alignItems: "center", mb: 2 }}>
              <FormControl fullWidth size="small">
                <InputLabel>Provider Service</InputLabel>
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
                <InputLabel>User Service</InputLabel>
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
            />
            <Box sx={{ display: "flex", gap: 1, alignItems: "center", mb: 2 }}>
              <FormControl fullWidth size="small">
                <InputLabel>Provider Service</InputLabel>
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
            />
            <TextField
              label="Holder Party"
              value={holderParty}
              onChange={(e) => setHolderParty(e.target.value)}
              size="small"
              fullWidth
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
            />
            <Box sx={{ display: "flex", gap: 1, alignItems: "center", mb: 2 }}>
              <FormControl fullWidth size="small">
                <InputLabel>User Service</InputLabel>
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
            />
            <TextField
              label="Credential ID"
              value={credentialId}
              onChange={(e) => setCredentialId(e.target.value)}
              size="small"
              fullWidth
              sx={{ mb: 2 }}
            />
            <TextField
              label="Credential Description"
              value={credentialDescription}
              onChange={(e) => setCredentialDescription(e.target.value)}
              size="small"
              fullWidth
              sx={{ mb: 2 }}
            />
            <Typography variant="caption" color="text.secondary">
              Claims
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
            />
            <Box sx={{ display: "flex", gap: 1, alignItems: "center", mb: 2 }}>
              <FormControl fullWidth size="small">
                <InputLabel>User Service</InputLabel>
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
              label="Credential Offer Contract ID"
              value={credentialOfferCid}
              onChange={(e) => setCredentialOfferCid(e.target.value)}
              size="small"
              fullWidth
            />
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
        {error && (
          <Alert severity="error" sx={{ mb: 2 }} onClose={() => setError(null)}>
            {error}
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
                label="Governance Contract ID"
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
                <InputLabel>Action Type</InputLabel>
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

              <Box sx={{ mt: 2, display: "flex", gap: 1 }}>
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
                {view !== "actions" && (
                  <Button
                    variant="outlined"
                    onClick={() => setShowNewActionForm(false)}
                  >
                    Cancel
                  </Button>
                )}
              </Box>
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
                <Select
                  value={proposalType}
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
                  {/*
                  Hidden per Notion "Clean up Utility Plugin" task — keep the
                  ProposalType variants + form fields + submit handlers wired
                  so existing API consumers still work; just not surfaced in
                  the dropdown for now.
                  <MenuItem value="provision_provider_service">Provision Provider Service</MenuItem>
                  <MenuItem value="create_delegated_batched_markers_proxy">Create Delegated Batched Markers Proxy</MenuItem>
                  */}
                  <ListSubheader sx={{ fontStyle: "italic", lineHeight: 1.5, pl: 4 }}>Actions</ListSubheader>
                  <MenuItem value="mint">Offer Mint</MenuItem>
                  <MenuItem value="burn">Offer Burn</MenuItem>
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

              {proposalType === "generic_vote" && (
                <TextField size="small" label="Vote Description" value={proposalDescription} onChange={(e) => setProposalDescription(e.target.value)} fullWidth required multiline minRows={2} maxRows={6} helperText="Describe what the governance members are voting on" />
              )}

              {proposalType === "setup_cc_preapproval" && (
                <>
                  {preapprovalCounts.cc > 0 && (
                    <Alert severity="warning">
                      This party already has a Canton Coin TransferPreapproval;
                      issuing another would create a duplicate and burn fees again.
                    </Alert>
                  )}
                  <TextField size="small" label="Provider Party" value={proposalProvider} onChange={(e) => setProposalProvider(e.target.value)} fullWidth required />
                  <TextField size="small" label="Expected DSO Party" value={proposalExpectedDso} onChange={(e) => setProposalExpectedDso(e.target.value)} fullWidth required />
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
                  <TextField size="small" label="Operator Party" value={proposalOperator} onChange={(e) => setProposalOperator(e.target.value)} fullWidth required />
                  <TextField size="small" label="Instrument Admin" value={proposalInstrumentAdmin} onChange={(e) => setProposalInstrumentAdmin(e.target.value)} fullWidth required />
                  <Typography variant="caption" sx={{ display: "block" }} color="text.secondary">
                    Instrument Allowances (optional)
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
                  <TextField size="small" label="TransferFactory Contract ID" value={proposalTransferFactoryCid} onChange={(e) => setProposalTransferFactoryCid(e.target.value)} fullWidth required />
                  <TextField size="small" label="Expected Admin Party" value={proposalExpectedAdmin} onChange={(e) => setProposalExpectedAdmin(e.target.value)} fullWidth required />
                  <TextField size="small" label="Receiver Party" value={proposalReceiver} onChange={(e) => setProposalReceiver(e.target.value)} fullWidth required />
                  <TextField size="small" label="Amount" value={proposalAmount} onChange={(e) => setProposalAmount(e.target.value)} fullWidth required />
                  <TextField size="small" label="Instrument Admin" value={proposalInstrumentIdAdmin} onChange={(e) => setProposalInstrumentIdAdmin(e.target.value)} fullWidth required />
                  <TextField size="small" label="Instrument ID" value={proposalInstrumentIdId} onChange={(e) => setProposalInstrumentIdId(e.target.value)} fullWidth required />
                  <TextField size="small" label="Input Holding CIDs (comma-separated)" value={proposalInputHoldingCids} onChange={(e) => setProposalInputHoldingCids(e.target.value)} fullWidth helperText="Leave empty for auto-selection" />
                </>
              )}

              {proposalType === "accept_transfer" && (
                <TextField size="small" label="TransferInstruction Contract ID" value={proposalTransferInstructionCid} onChange={(e) => setProposalTransferInstructionCid(e.target.value)} fullWidth required />
              )}

              {proposalType === "provision_provider_service" && (
                <Typography variant="caption" color="text.secondary">
                  Provisions a Utility-Registry ProviderService with operator = proposer and provider = governance party. No parameters required.
                </Typography>
              )}

              {proposalType === "setup_utility" && (
                <>
                  <FormControl size="small" fullWidth required>
                    <InputLabel>ProviderService</InputLabel>
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
                  <TextField size="small" label="Operator Party" value={proposalOperator} onChange={(e) => setProposalOperator(e.target.value)} fullWidth required />
                  <TextField size="small" label="Instrument ID" value={proposalInstrumentIdText} onChange={(e) => setProposalInstrumentIdText(e.target.value)} fullWidth required />
                  <FormControlLabel
                    control={<Checkbox size="small" checked={proposalCreateTransferRule} onChange={(e) => setProposalCreateTransferRule(e.target.checked)} />}
                    label="Create TransferRule"
                  />
                  <FormControlLabel
                    control={<Checkbox size="small" checked={proposalCreateAllocationFactory} onChange={(e) => setProposalCreateAllocationFactory(e.target.checked)} />}
                    label="Create AllocationFactory"
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
                  <TextField size="small" label="Operator Party" value={proposalOperator} onChange={(e) => setProposalOperator(e.target.value)} fullWidth required />
                  <TextField size="small" label="Provider Party" value={proposalProvider} onChange={(e) => setProposalProvider(e.target.value)} fullWidth required />
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
                  <TextField size="small" label="Operator Party" value={proposalOperator} onChange={(e) => setProposalOperator(e.target.value)} fullWidth required />
                  <TextField size="small" label="User Party" value={proposalUser} onChange={(e) => setProposalUser(e.target.value)} fullWidth required />
                </>
              )}

              {proposalType === "set_provider_app_reward_beneficiaries" && (
                <>
                  <FormControl size="small" fullWidth required>
                    <InputLabel>InstrumentConfiguration</InputLabel>
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
                    label="Clear beneficiaries (set to None)"
                  />
                  {!proposalClearBeneficiaries && (
                    <TextField
                      size="small"
                      label="Beneficiaries (one per line: party,weight)"
                      value={proposalBeneficiariesText}
                      onChange={(e) => setProposalBeneficiariesText(e.target.value)}
                      fullWidth
                      multiline
                      minRows={2}
                      maxRows={6}
                      helperText="Each line: <party>,<weight>"
                    />
                  )}
                </>
              )}

              {proposalType === "set_enable_result_contracts" && (
                <>
                  <FormControl size="small" fullWidth required>
                    <InputLabel>RegistrarService</InputLabel>
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
                    <InputLabel>Enable Result Contracts</InputLabel>
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
                <TextField size="small" label="Operator Party" value={proposalOperator} onChange={(e) => setProposalOperator(e.target.value)} fullWidth required />
              )}

              {(proposalType === "mint" || proposalType === "burn") && (
                <>
                  <FormControl size="small" fullWidth required>
                    <InputLabel>Instrument</InputLabel>
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
                    <InputLabel>AllocationFactory</InputLabel>
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
                  <TextField size="small" label={proposalType === "mint" ? "Recipient Party" : "Holder Party"} value={proposalType === "mint" ? proposalRecipient : proposalHolder} onChange={(e) => proposalType === "mint" ? setProposalRecipient(e.target.value) : setProposalHolder(e.target.value)} fullWidth required />
                  <TextField size="small" label="Amount" value={proposalAmount} onChange={(e) => setProposalAmount(e.target.value)} fullWidth required />
                  <TextField size="small" label="Description" value={proposalDescription} onChange={(e) => setProposalDescription(e.target.value)} fullWidth required />
                </>
              )}

              {(proposalType === "accept_mint_request" || proposalType === "accept_burn_request") && (() => {
                const isMint = proposalType === "accept_mint_request";
                const requestContracts = isMint ? mintRequestContracts : burnRequestContracts;
                const requestCid = isMint ? proposalMintRequestCid : proposalBurnRequestCid;
                const setRequestCid = isMint ? setProposalMintRequestCid : setProposalBurnRequestCid;
                const requestLabel = isMint ? "MintRequest" : "BurnRequest";
                return (
                  <>
                    <FormControl size="small" fullWidth required>
                      <InputLabel>{requestLabel}</InputLabel>
                      <Select
                        label={requestLabel}
                        value={requestCid}
                        onChange={(e) => setRequestCid(e.target.value)}
                        MenuProps={{ disableScrollLock: true }}
                      >
                        {requestContracts.length > 0 ? (
                          requestContracts.map((c) => (
                            <MenuItem key={c.contract_id} value={c.contract_id}>
                              {c.contract_id.slice(0, 16)}…
                            </MenuItem>
                          ))
                        ) : (
                          <MenuItem disabled>
                            No {requestLabel} contracts found — holder must create one first
                          </MenuItem>
                        )}
                      </Select>
                    </FormControl>
                    <FormControl size="small" fullWidth required>
                      <InputLabel>Instrument</InputLabel>
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
                    <TextField size="small" label="Description" value={proposalDescription} onChange={(e) => setProposalDescription(e.target.value)} fullWidth required />
                  </>
                );
              })()}

              {proposalType === "offer_free_credential" && (
                <>
                  <TextField size="small" label="UserService Contract ID" value={proposalUserServiceCid} onChange={(e) => setProposalUserServiceCid(e.target.value)} fullWidth required helperText="Governance party's UserService cid" />
                  <TextField size="small" label="Holder Party" value={proposalHolder} onChange={(e) => setProposalHolder(e.target.value)} fullWidth required />
                  <TextField size="small" label="Credential ID" value={proposalCredentialId} onChange={(e) => setProposalCredentialId(e.target.value)} fullWidth required />
                  <TextField size="small" label="Description" value={proposalDescription} onChange={(e) => setProposalDescription(e.target.value)} fullWidth required />
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
                  />
                </>
              )}

              {proposalType === "accept_free_credential" && (
                <>
                  <TextField size="small" label="UserService Contract ID" value={proposalUserServiceCid} onChange={(e) => setProposalUserServiceCid(e.target.value)} fullWidth required helperText="Governance party's UserService cid" />
                  <TextField size="small" label="CredentialOffer Contract ID" value={proposalCredentialOfferCid} onChange={(e) => setProposalCredentialOfferCid(e.target.value)} fullWidth required />
                </>
              )}

              {proposalType === "offer_paid_credential" && (
                <Typography variant="caption" color="text.secondary">
                  Paid credential proposal form is not implemented yet. Use the Free direction or call <code>POST /governance/propose</code> directly with a <code>type: "offer_paid_credential"</code> payload.
                </Typography>
              )}

              <Button variant="contained" size="small" onClick={handleSubmitProposal} disabled={proposalLoading || proposalType === "offer_paid_credential"}>
                {proposalLoading ? <CircularProgress size={16} /> : "Submit Proposal"}
              </Button>
            </Box>
          </Collapse>
        </Box>
      )}

    </Box>
  );
};
