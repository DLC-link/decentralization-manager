import { useState, useEffect, useCallback, useMemo } from "react";
import {
  Autocomplete,
  Box,
  Typography,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableRow,
  Button,
  Chip,
  CircularProgress,
  Alert,
  Collapse,
  IconButton,
  TextField,
  Select,
  MenuItem,
  FormControl,
  InputLabel,
  Divider,
  Checkbox,
  FormControlLabel,
  FormGroup,
} from "@mui/material";
import ExpandMoreIcon from "@mui/icons-material/ExpandMore";
import ExpandLessIcon from "@mui/icons-material/ExpandLess";
import CheckCircleIcon from "@mui/icons-material/CheckCircle";
import PlayArrowIcon from "@mui/icons-material/PlayArrow";
import AddIcon from "@mui/icons-material/Add";
import UndoIcon from "@mui/icons-material/Undo";
import RefreshIcon from "@mui/icons-material/Refresh";
import TimerOffIcon from "@mui/icons-material/TimerOff";
import { ExecuteDialog } from "./ExecuteDialog";
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
import { zebraRow } from "../styles";
import type {
  GovernanceResponse,
  GovernanceAction,
  ActionType,
  ConfirmActionRequest,
  ExecuteActionRequest,
  CancelConfirmationRequest,
  ExpireConfirmationRequest,
  DisclosedContractInput,
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
  DomainGovernanceAction,
  Network,
  NetworkInfo,
  ProposeActionRequest,
  ProposalType,
} from "../types";

// Action types — ordered per GOVERNANCE_CLIENT_MIGRATION.md vault launch sequence
// Hidden entries are kept for type safety and display of existing actions
const getActionTypeOptions = (
  network?: Network,
): { value: ActionType["type"]; label: string; hidden?: boolean }[] => [
  // Step 1: Utility Registry Onboarding
  {
    value: "utility_create_provider_request",
    label: "Create Provider Service",
  },
  { value: "utility_create_user_request", label: "Create User Service" },
  { value: "utility_setup", label: "Setup Utility" },
  // Feature App (needed before vault deployment for FAR config, devnet only)
  {
    value: "dev_net_feature_app",
    label: "DevNet: Feature App",
    hidden: network !== "devnet",
  },
  // Deploy Vault
  { value: "vault_deployment", label: "Deploy Vault" },
  // Deploy YieldEpoch
  { value: "yield_epoch_deployment", label: "Deploy YieldEpoch" },
  // Request Processor Deployment
  {
    value: "processor_deployment_request",
    label: "Request Processor Deployment",
  },
  // Accept Holder Service Requests
  {
    value: "utility_accept_holder_service_request",
    label: "Accept Holder Service",
    hidden: true,
  },
  // Hidden — not in current vault launch sequence
  {
    value: "governance_add_member",
    label: "Add Governance Member",
    hidden: true,
  },
  {
    value: "governance_remove_member",
    label: "Remove Governance Member",
    hidden: true,
  },
  {
    value: "governance_set_threshold",
    label: "Set Governance Threshold",
    hidden: true,
  },
  {
    value: "governance_set_timeout",
    label: "Set Governance Timeout",
    hidden: true,
  },
  { value: "vault_pause", label: "Pause Vault" },
  { value: "vault_unpause", label: "Unpause Vault" },
  { value: "vault_update_limits", label: "Update Vault Limits", hidden: true },
  {
    value: "vault_update_backend",
    label: "Update Vault Backend",
    hidden: true,
  },
  {
    value: "vault_update_far_beneficiaries",
    label: "Update FAR Beneficiaries",
    hidden: true,
  },
  {
    value: "credential_offer_free",
    label: "Credential: Offer Free",
    hidden: true,
  },
  {
    value: "credential_accept_free",
    label: "Credential: Accept Free",
  },
];

type ActionTypeKey = ActionType["type"];

// All action type options (network-independent, for label lookup)
const ALL_ACTION_TYPE_OPTIONS = getActionTypeOptions();

// Format an ActionType for display
const formatActionType = (action: ActionType): string => {
  const label =
    ALL_ACTION_TYPE_OPTIONS.find((opt) => opt.value === action.type)?.label ||
    action.type;
  return label;
};

interface GovernanceSectionProps {
  partyId: string;
  rulesContractId?: string;
  governanceContractIds?: string[];
  memberPartyId?: string;
  defaultOperatorParty?: string;
  network?: Network;
  governanceType?: "vault" | "core_self" | "core_domain";
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
  memberPartyId,
  defaultOperatorParty,
  network,
  governanceType = "vault",
}: GovernanceSectionProps) => {
  const [expanded, setExpanded] = useState(true);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [data, setData] = useState<GovernanceResponse | null>(null);
  const [actionLoading, setActionLoading] = useState<string | null>(null);
  const [executeDialogAction, setExecuteDialogAction] =
    useState<GovernanceAction | null>(null);
  const [executeError, setExecuteError] = useState<string | null>(null);
  // Domain proposal state
  const [showProposalForm, setShowProposalForm] = useState(false);
  const [proposalType, setProposalType] = useState<ProposalType["type"]>("setup_cc_preapproval");
  const [proposalProvider, setProposalProvider] = useState("");
  const [proposalExpectedDso, setProposalExpectedDso] = useState("");
  const [proposalOperator, setProposalOperator] = useState("");
  const [proposalInstrumentAdmin, setProposalInstrumentAdmin] = useState("");
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
  const [proposalLoading, setProposalLoading] = useState(false);
  const [rulesContractId, setRulesContractId] = useState(
    initialRulesContractId || "",
  );

  // Action form state
  const [showNewActionForm, setShowNewActionForm] = useState(false);
  const [selectedActionType, setSelectedActionType] = useState<ActionTypeKey>(
    "utility_create_provider_request",
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
  const [servicesLoading, setServicesLoading] = useState(false);

  // Contracts fetched by template (with blobs)
  const [vaultRulesContracts, setVaultRulesContracts] = useState<ContractWithBlob[]>([]);
  const [allocationFactoryContracts, setAllocationFactoryContracts] = useState<ContractWithBlob[]>([]);
  const [featuredAppRightContracts, setFeaturedAppRightContracts] = useState<ContractWithBlob[]>([]);
  const [amuletRulesContract, setAmuletRulesContract] = useState<ContractWithBlob | null>(null);
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
        setAmuletRulesContract({
          contract_id: data.amulet_rules_cid,
          blob: data.amulet_rules_blob,
        });
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

  // Build dynamic blob map from queried contracts (for ExecuteDialog)
  const contractBlobMap = useMemo(() => {
    const map: Record<string, string> = {};
    for (const c of [
      ...vaultRulesContracts,
      ...allocationFactoryContracts,
      ...featuredAppRightContracts,
      ...registrarServiceContracts,
    ]) {
      if (c.blob) map[c.contract_id] = c.blob;
    }
    if (burnMintFactory?.blob) {
      map[burnMintFactory.contract_id] = burnMintFactory.blob;
    }
    if (amuletRulesContract?.blob) {
      map[amuletRulesContract.contract_id] = amuletRulesContract.blob;
    }
    return map;
  }, [vaultRulesContracts, allocationFactoryContracts, featuredAppRightContracts, registrarServiceContracts, burnMintFactory, amuletRulesContract]);

  const handleConfirm = async (action: GovernanceAction) => {
    if (!rulesContractId) {
      setError("Please enter the Governance contract ID");
      return;
    }

    setActionLoading(action.action_hash);
    setError(null);

    try {
      const request: ConfirmActionRequest = {
        party_id: partyId,
        rules_contract_id: rulesContractId,
        action: action.action,
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

      // Refresh data
      await fetchGovernance();
    } catch (e) {
      setError(
        e instanceof Error ? e.message : "Failed to submit confirmation",
      );
    } finally {
      setActionLoading(null);
    }
  };

  const handleExecute = async (
    action: GovernanceAction,
    disclosedContracts: DisclosedContractInput[],
  ) => {
    if (!rulesContractId) {
      setExecuteError("Please enter the Governance contract ID");
      return;
    }

    setActionLoading(action.action_hash);
    setExecuteError(null);

    try {
      const request: ExecuteActionRequest = {
        party_id: partyId,
        rules_contract_id: rulesContractId,
        action: action.action,
        confirmation_cids: action.confirmations.map((c) => c.contract_id),
        disclosed_contracts: disclosedContracts,
        governance_type: governanceType,
      };

      const res = await authenticatedFetch(`${API_BASE}/governance/execute`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(request),
      });

      if (!res.ok) {
        const errData = await res.json().catch(() => ({}));
        throw new Error(errData.error || "Failed to execute action");
      }

      // Close dialog and refresh data
      setExecuteDialogAction(null);
      await fetchGovernance();
    } catch (e) {
      setExecuteError(
        e instanceof Error ? e.message : "Failed to execute action",
      );
    } finally {
      setActionLoading(null);
    }
  };

  const handleRevoke = async (
    action: GovernanceAction,
    confirmationCid: string,
  ) => {
    setActionLoading(action.action_hash);
    setError(null);

    try {
      const request: CancelConfirmationRequest = {
        party_id: partyId,
        confirmation_cid: confirmationCid,
        governance_type: governanceType,
      };

      const res = await authenticatedFetch(`${API_BASE}/governance/cancel`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(request),
      });

      if (!res.ok) {
        const errData = await res.json().catch(() => ({}));
        throw new Error(errData.error || "Failed to revoke confirmation");
      }

      // Refresh data
      await fetchGovernance();
    } catch (e) {
      setError(
        e instanceof Error ? e.message : "Failed to revoke confirmation",
      );
    } finally {
      setActionLoading(null);
    }
  };

  const handleExpire = async (
    action: GovernanceAction,
    confirmationCid: string,
  ) => {
    if (!rulesContractId) {
      setError("Please enter the Governance contract ID");
      return;
    }

    setActionLoading(action.action_hash);
    setError(null);

    try {
      const request: ExpireConfirmationRequest = {
        party_id: partyId,
        rules_contract_id: rulesContractId,
        confirmation_cid: confirmationCid,
        governance_type: governanceType,
      };

      const res = await authenticatedFetch(`${API_BASE}/governance/expire`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(request),
      });

      if (!res.ok) {
        const errData = await res.json().catch(() => ({}));
        throw new Error(errData.error || "Failed to expire confirmation");
      }

      await fetchGovernance();
    } catch (e) {
      setError(
        e instanceof Error ? e.message : "Failed to expire confirmation",
      );
    } finally {
      setActionLoading(null);
    }
  };

  // Helper to find the current user's confirmation for an action
  // confirming_party is the member party ID, not the decentralized party ID
  // Use member_party_id from response (primary) or prop (fallback)
  const getUserConfirmation = (action: GovernanceAction) => {
    const myMemberPartyId = data?.member_party_id || memberPartyId;
    if (!myMemberPartyId) return undefined;
    return action.confirmations.find(
      (c) => c.confirming_party === myMemberPartyId,
    );
  };

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

      // Reset form and refresh data
      setShowNewActionForm(false);
      await fetchGovernance();
    } catch (e) {
      setError(
        e instanceof Error ? e.message : "Failed to submit confirmation",
      );
    } finally {
      setFormLoading(false);
    }
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
            instrument_allowances: [],
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

      setShowProposalForm(false);
      await fetchGovernance();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to create proposal");
    } finally {
      setProposalLoading(false);
    }
  };

  const handleConfirmDomain = async (domainAction: DomainGovernanceAction) => {
    if (!rulesContractId) return;
    setActionLoading(domainAction.proposal_cid);
    setError(null);

    try {
      const res = await authenticatedFetch(`${API_BASE}/governance/confirm`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          party_id: partyId,
          rules_contract_id: rulesContractId,
          action: { type: "governance_set_threshold", new_threshold: 0 }, // placeholder
          governance_type: "core_domain",
          proposal_cid: domainAction.proposal_cid,
        }),
      });

      if (!res.ok) {
        const errData = await res.json().catch(() => ({}));
        throw new Error(errData.error || "Failed to confirm proposal");
      }

      await fetchGovernance();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to confirm proposal");
    } finally {
      setActionLoading(null);
    }
  };

  const handleExecuteDomain = async (domainAction: DomainGovernanceAction) => {
    if (!rulesContractId) return;
    setActionLoading(domainAction.proposal_cid);
    setError(null);

    try {
      const res = await authenticatedFetch(`${API_BASE}/governance/execute`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          party_id: partyId,
          rules_contract_id: rulesContractId,
          action: { type: "governance_set_threshold", new_threshold: 0 }, // placeholder
          governance_type: "core_domain",
          proposal_cid: domainAction.proposal_cid,
          confirmation_cids: domainAction.confirmations.map((c) => c.contract_id),
          disclosed_contracts: [],
        }),
      });

      if (!res.ok) {
        const errData = await res.json().catch(() => ({}));
        throw new Error(errData.error || "Failed to execute proposal");
      }

      await fetchGovernance();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to execute proposal");
    } finally {
      setActionLoading(null);
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
              <IconButton
                size="small"
                onClick={fetchDeployContracts}
                disabled={deployContractsLoading}
              >
                {deployContractsLoading ? <CircularProgress size={20} /> : <RefreshIcon />}
              </IconButton>
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
            <Typography variant="caption" display="block" color="text.secondary">
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
              <IconButton
                size="small"
                onClick={fetchDeployContracts}
                disabled={deployContractsLoading}
              >
                {deployContractsLoading ? <CircularProgress size={20} /> : <RefreshIcon />}
              </IconButton>
            </Box>
            <Box sx={{ mb: 2 }}>
              <Typography variant="caption" display="block" color="text.secondary" sx={{ mb: 1 }}>
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
              <IconButton
                size="small"
                onClick={fetchDeployContracts}
                disabled={deployContractsLoading}
              >
                {deployContractsLoading ? <CircularProgress size={20} /> : <RefreshIcon />}
              </IconButton>
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
              <IconButton
                size="small"
                onClick={fetchDeployContracts}
                disabled={deployContractsLoading}
              >
                {deployContractsLoading ? <CircularProgress size={20} /> : <RefreshIcon />}
              </IconButton>
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
              <IconButton
                size="small"
                onClick={fetchDeployContracts}
                disabled={deployContractsLoading}
              >
                {deployContractsLoading ? <CircularProgress size={20} /> : <RefreshIcon />}
              </IconButton>
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
            <Typography variant="caption" display="block" color="text.secondary">
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
              <IconButton
                size="small"
                onClick={fetchBurnMintFactory}
                disabled={burnMintFactoryLoading}
              >
                {burnMintFactoryLoading ? <CircularProgress size={20} /> : <RefreshIcon />}
              </IconButton>
            </Box>
            <Typography variant="caption" display="block" color="text.secondary">
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
              <IconButton
                size="small"
                onClick={fetchDeployContracts}
                disabled={deployContractsLoading}
              >
                {deployContractsLoading ? <CircularProgress size={20} /> : <RefreshIcon />}
              </IconButton>
            </Box>
            <Box sx={{ mb: 2 }}>
              <Typography variant="caption" display="block" color="text.secondary" sx={{ mb: 1 }}>
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
            <Typography variant="caption" display="block" color="text.secondary">
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
              <IconButton
                size="small"
                onClick={fetchServices}
                disabled={servicesLoading}
              >
                {servicesLoading ? <CircularProgress size={20} /> : <RefreshIcon />}
              </IconButton>
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
              <IconButton
                size="small"
                onClick={fetchServices}
                disabled={servicesLoading}
              >
                {servicesLoading ? <CircularProgress size={20} /> : <RefreshIcon />}
              </IconButton>
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
              <IconButton
                size="small"
                onClick={fetchServices}
                disabled={servicesLoading}
              >
                {servicesLoading ? <CircularProgress size={20} /> : <RefreshIcon />}
              </IconButton>
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
              <IconButton
                size="small"
                onClick={fetchServices}
                disabled={servicesLoading}
              >
                {servicesLoading ? <CircularProgress size={20} /> : <RefreshIcon />}
              </IconButton>
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
              <IconButton
                size="small"
                onClick={fetchServices}
                disabled={servicesLoading}
              >
                {servicesLoading ? <CircularProgress size={20} /> : <RefreshIcon />}
              </IconButton>
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
            <IconButton
              size="small"
              onClick={fetchNetworkInfo}
              disabled={amuletRulesLoading}
            >
              {amuletRulesLoading ? <CircularProgress size={20} /> : <RefreshIcon />}
            </IconButton>
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

      <Collapse in={expanded}>
        {error && (
          <Alert severity="error" sx={{ mb: 2 }} onClose={() => setError(null)}>
            {error}
          </Alert>
        )}

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

        {/* New Action Form */}
        <Box sx={{ mb: 2 }}>
          <Button
            size="small"
            variant="outlined"
            startIcon={showNewActionForm ? <ExpandLessIcon /> : <AddIcon />}
            onClick={() => setShowNewActionForm(!showNewActionForm)}
            disabled={!ADMIN_ACCESS || !rulesContractId}
          >
            {showNewActionForm ? "Hide Form" : "New Governance Action"}
          </Button>

          <Collapse in={showNewActionForm}>
            <Box
              sx={{
                mt: 2,
                p: 2,
                border: "1px solid",
                borderColor: "divider",
                borderRadius: 1,
              }}
            >
              <Typography variant="subtitle2" sx={{ mb: 2 }}>
                Create New Governance Action
              </Typography>

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
                <Button
                  variant="outlined"
                  onClick={() => setShowNewActionForm(false)}
                >
                  Cancel
                </Button>
              </Box>
            </Box>
          </Collapse>
        </Box>

        {data && data.actions.length > 0 ? (
          <Box sx={{ overflowX: "auto", mx: -2 }}>
            <Table size="small">
              <TableHead>
                <TableRow>
                  <TableCell sx={{ py: 1 }}>Action</TableCell>
                  <TableCell sx={{ py: 1 }}>Confirmations</TableCell>
                  <TableCell sx={{ py: 1 }} align="right">
                    Actions
                  </TableCell>
                </TableRow>
              </TableHead>
              <TableBody>
                {data.actions.map((action, idx) => (
                  <TableRow key={action.action_hash} sx={zebraRow(idx)}>
                    <TableCell sx={{ py: 1 }}>
                      <Typography variant="body2" sx={{ fontWeight: 500 }}>
                        {formatActionType(action.action)}
                      </Typography>
                      <Typography
                        variant="caption"
                        color="text.secondary"
                        sx={{ fontFamily: "monospace" }}
                      >
                        {action.action_hash}
                      </Typography>
                    </TableCell>
                    <TableCell sx={{ py: 1 }}>
                      <Box sx={{ display: "flex", alignItems: "center", gap: 1, mb: action.confirmations.length > 0 ? 0.5 : 0 }}>
                        <Chip
                          label={`${action.confirmation_count} / ${data.threshold}`}
                          size="small"
                          color={action.can_execute ? "success" : "default"}
                        />
                      </Box>
                      {action.confirmations.map((conf) => {
                        const isOwn = conf.confirming_party === (data?.member_party_id || memberPartyId);
                        return (
                          <Box
                            key={conf.contract_id}
                            sx={{ display: "flex", alignItems: "center", gap: 0.5, mt: 0.5 }}
                          >
                            <Typography
                              variant="caption"
                              sx={{
                                fontFamily: "monospace",
                                color: isOwn ? "primary.main" : "text.secondary",
                              }}
                            >
                              {conf.confirming_party.length > 20
                                ? `${conf.confirming_party.slice(0, 10)}...${conf.confirming_party.slice(-8)}`
                                : conf.confirming_party}
                              {isOwn ? " (you)" : ""}
                            </Typography>
                            {!isOwn && ADMIN_ACCESS && rulesContractId && (
                              <IconButton
                                size="small"
                                title="Expire confirmation"
                                onClick={() => handleExpire(action, conf.contract_id)}
                                disabled={actionLoading === action.action_hash}
                                sx={{ p: 0.25 }}
                              >
                                <TimerOffIcon sx={{ fontSize: 14 }} />
                              </IconButton>
                            )}
                          </Box>
                        );
                      })}
                    </TableCell>
                    <TableCell sx={{ py: 1 }} align="right">
                      <Box
                        sx={{
                          display: "flex",
                          gap: 1,
                          justifyContent: "flex-end",
                        }}
                      >
                        {getUserConfirmation(action) ? (
                          <Button
                            size="small"
                            variant="outlined"
                            color="warning"
                            startIcon={
                              actionLoading === action.action_hash ? (
                                <CircularProgress size={16} />
                              ) : (
                                <UndoIcon />
                              )
                            }
                            onClick={() => {
                              const confirmation = getUserConfirmation(action);
                              if (confirmation) {
                                handleRevoke(action, confirmation.contract_id);
                              }
                            }}
                            disabled={
                              !ADMIN_ACCESS ||
                              actionLoading === action.action_hash
                            }
                          >
                            Revoke
                          </Button>
                        ) : (
                          <Button
                            size="small"
                            variant="outlined"
                            startIcon={
                              actionLoading === action.action_hash ? (
                                <CircularProgress size={16} />
                              ) : (
                                <CheckCircleIcon />
                              )
                            }
                            onClick={() => handleConfirm(action)}
                            disabled={
                              !ADMIN_ACCESS ||
                              !rulesContractId ||
                              actionLoading === action.action_hash
                            }
                          >
                            Confirm
                          </Button>
                        )}
                        {action.can_execute && (
                          <Button
                            size="small"
                            variant="contained"
                            color="success"
                            startIcon={
                              actionLoading === action.action_hash ? (
                                <CircularProgress size={16} color="inherit" />
                              ) : (
                                <PlayArrowIcon />
                              )
                            }
                            onClick={() => {
                              setExecuteError(null);
                              setExecuteDialogAction(action);
                            }}
                            disabled={
                              !ADMIN_ACCESS ||
                              !rulesContractId ||
                              actionLoading === action.action_hash
                            }
                          >
                            Execute
                          </Button>
                        )}
                      </Box>
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </Box>
        ) : (
          <Typography variant="body2" color="text.secondary" sx={{ pt: 2, pb: 2 }}>
            No governance actions found for this party.
          </Typography>
        )}
      </Collapse>

      {/* Domain Proposals — only for governance-core */}
      {governanceType === "core_self" && data && (
        <Box sx={{ mt: 2, mx: -2 }}>
          <Box sx={{ display: "flex", justifyContent: "space-between", alignItems: "center", mb: 1, px: 2 }}>
            <Typography variant="subtitle2">
              Domain Proposals
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

          <Collapse in={showProposalForm}>
            <Box sx={{ display: "flex", flexDirection: "column", gap: 1.5, mb: 2, p: 2, mx: 2, border: 1, borderColor: "divider", borderRadius: 2 }}>
              <FormControl size="small" fullWidth>
                <Select
                  value={proposalType}
                  onChange={(e) => setProposalType(e.target.value as ProposalType["type"])}
                >
                  <MenuItem value="generic_vote">Generic Vote</MenuItem>
                  <MenuItem value="setup_cc_preapproval">Setup CC Preapproval</MenuItem>
                  <MenuItem value="setup_token_preapproval">Setup Token Preapproval</MenuItem>
                  <MenuItem value="transfer">Transfer</MenuItem>
                  <MenuItem value="accept_transfer">Accept Transfer</MenuItem>
                  <Divider />
                  <MenuItem value="provision_provider_service">Provision Provider Service</MenuItem>
                  <MenuItem value="setup_utility">Setup Utility</MenuItem>
                  <MenuItem value="create_provider_service_request">Create Provider Service Request</MenuItem>
                  <MenuItem value="create_user_service_request">Create User Service Request</MenuItem>
                  <MenuItem value="set_provider_app_reward_beneficiaries">Set Provider App Reward Beneficiaries</MenuItem>
                  <MenuItem value="set_enable_result_contracts">Set Enable Result Contracts</MenuItem>
                  <MenuItem value="create_delegated_batched_markers_proxy">Create Delegated Batched Markers Proxy</MenuItem>
                  <MenuItem value="mint">Mint</MenuItem>
                  <MenuItem value="burn">Burn</MenuItem>
                </Select>
              </FormControl>

              {proposalType === "generic_vote" && (
                <TextField size="small" label="Vote Description" value={proposalDescription} onChange={(e) => setProposalDescription(e.target.value)} fullWidth required multiline minRows={2} maxRows={6} helperText="Describe what the governance members are voting on" />
              )}

              {proposalType === "setup_cc_preapproval" && (
                <>
                  <TextField size="small" label="Provider Party" value={proposalProvider} onChange={(e) => setProposalProvider(e.target.value)} fullWidth required />
                  <TextField size="small" label="Expected DSO Party" value={proposalExpectedDso} onChange={(e) => setProposalExpectedDso(e.target.value)} fullWidth required />
                </>
              )}

              {proposalType === "setup_token_preapproval" && (
                <>
                  <TextField size="small" label="Operator Party" value={proposalOperator} onChange={(e) => setProposalOperator(e.target.value)} fullWidth required />
                  <TextField size="small" label="Instrument Admin" value={proposalInstrumentAdmin} onChange={(e) => setProposalInstrumentAdmin(e.target.value)} fullWidth required />
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
                  <TextField size="small" label="ProviderService Contract ID" value={proposalProviderServiceCid} onChange={(e) => setProposalProviderServiceCid(e.target.value)} fullWidth required />
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
                  <TextField size="small" label="Operator Party" value={proposalOperator} onChange={(e) => setProposalOperator(e.target.value)} fullWidth required />
                  <TextField size="small" label="Provider Party" value={proposalProvider} onChange={(e) => setProposalProvider(e.target.value)} fullWidth required />
                </>
              )}

              {proposalType === "create_user_service_request" && (
                <>
                  <TextField size="small" label="Operator Party" value={proposalOperator} onChange={(e) => setProposalOperator(e.target.value)} fullWidth required />
                  <TextField size="small" label="User Party" value={proposalUser} onChange={(e) => setProposalUser(e.target.value)} fullWidth required />
                </>
              )}

              {proposalType === "set_provider_app_reward_beneficiaries" && (
                <>
                  <TextField size="small" label="InstrumentConfiguration Contract ID" value={proposalInstrumentConfigurationCid} onChange={(e) => setProposalInstrumentConfigurationCid(e.target.value)} fullWidth required />
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
                  <TextField size="small" label="RegistrarService Contract ID" value={proposalRegistrarServiceCid} onChange={(e) => setProposalRegistrarServiceCid(e.target.value)} fullWidth required />
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
                  <TextField size="small" label="AllocationFactory Contract ID" value={proposalAllocationFactoryCid} onChange={(e) => setProposalAllocationFactoryCid(e.target.value)} fullWidth required />
                  <TextField size="small" label="Instrument Admin" value={proposalInstrumentIdAdmin} onChange={(e) => setProposalInstrumentIdAdmin(e.target.value)} fullWidth required />
                  <TextField size="small" label="Instrument ID" value={proposalInstrumentIdId} onChange={(e) => setProposalInstrumentIdId(e.target.value)} fullWidth required />
                  <TextField size="small" label="InstrumentConfiguration Contract ID" value={proposalInstrumentConfigurationCid} onChange={(e) => setProposalInstrumentConfigurationCid(e.target.value)} fullWidth required />
                  <TextField size="small" label={proposalType === "mint" ? "Recipient Party" : "Holder Party"} value={proposalType === "mint" ? proposalRecipient : proposalHolder} onChange={(e) => proposalType === "mint" ? setProposalRecipient(e.target.value) : setProposalHolder(e.target.value)} fullWidth required />
                  <TextField size="small" label="Amount" value={proposalAmount} onChange={(e) => setProposalAmount(e.target.value)} fullWidth required />
                  <TextField size="small" label="Description" value={proposalDescription} onChange={(e) => setProposalDescription(e.target.value)} fullWidth required />
                </>
              )}

              <Button variant="contained" size="small" onClick={handleSubmitProposal} disabled={proposalLoading}>
                {proposalLoading ? <CircularProgress size={16} /> : "Submit Proposal"}
              </Button>
            </Box>
          </Collapse>

          {(data.domain_actions?.length ?? 0) > 0 ? (
            <Table size="small">
              <TableHead>
                <TableRow>
                  <TableCell>Proposal</TableCell>
                  <TableCell>Confirmations</TableCell>
                  <TableCell align="right">Actions</TableCell>
                </TableRow>
              </TableHead>
              <TableBody>
                {data.domain_actions!.map((da, idx) => (
                  <TableRow key={da.proposal_cid} sx={zebraRow(idx)}>
                    <TableCell>
                      <Typography variant="body2">{da.action_label}</Typography>
                      {da.description && (
                        <Typography variant="caption" color="text.primary" sx={{ display: "block" }}>
                          {da.description}
                        </Typography>
                      )}
                      <Typography variant="caption" color="text.secondary" sx={{ fontFamily: "monospace" }}>
                        {da.proposal_cid.slice(0, 16)}...
                      </Typography>
                    </TableCell>
                    <TableCell>
                      {da.confirmation_count} / {data.threshold}
                    </TableCell>
                    <TableCell align="right">
                      <Box sx={{ display: "flex", gap: 0.5, justifyContent: "flex-end" }}>
                        {!da.confirmations.some((c) => c.confirming_party === (data.member_party_id || memberPartyId)) && (
                          <Button
                            size="small"
                            variant="outlined"
                            onClick={() => handleConfirmDomain(da)}
                            disabled={actionLoading === da.proposal_cid}
                          >
                            Confirm
                          </Button>
                        )}
                        {da.can_execute && (
                          <Button
                            size="small"
                            variant="contained"
                            onClick={() => handleExecuteDomain(da)}
                            disabled={actionLoading === da.proposal_cid}
                          >
                            Execute
                          </Button>
                        )}
                      </Box>
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          ) : (
            !showProposalForm && (
              <Typography variant="body2" color="text.secondary" sx={{ px: 2, pb: 2 }}>
                No domain proposals pending.
              </Typography>
            )
          )}
        </Box>
      )}

      <ExecuteDialog
        open={executeDialogAction !== null}
        onClose={() => setExecuteDialogAction(null)}
        onExecute={(disclosedContracts) => {
          if (executeDialogAction) {
            handleExecute(executeDialogAction, disclosedContracts);
          }
        }}
        action={executeDialogAction}
        loading={actionLoading === executeDialogAction?.action_hash}
        error={executeError}
        blobMap={contractBlobMap}
      />
    </Box>
  );
};
