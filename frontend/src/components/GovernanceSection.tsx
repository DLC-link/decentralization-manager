import { useState, useEffect, useCallback } from "react";
import {
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
import { ExecuteDialog } from "./ExecuteDialog";
import {
  API_BASE,
  ADMIN_ACCESS,
  MAX_TOTAL_DEPOSIT,
  MIN_DEPOSIT_AMOUNT,
  MIN_WITHDRAWAL_AMOUNT,
  DEVNET_VAULT_RULES_CID,
  DEVNET_FEATURED_APP_RIGHT_CID,
  DEVNET_VAULT_BACKEND_SIGNATORY,
  DEVNET_AMULET_RULES_CID,
  DEVNET_CBTC_DEC_PARTY,
  DEVNET_VAULT_PROCESSOR_RULES_CID,
  DEVNET_OPERATOR,
  DEVNET_ALLOCATION_FACTORY_CID,
} from "../constants";
import type {
  GovernanceResponse,
  GovernanceAction,
  ActionType,
  ConfirmActionRequest,
  ExecuteActionRequest,
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
  RegistrarServiceInfo,
  RegistrarServicesResponse,
} from "../types";

// Action type labels for display
const ACTION_TYPE_OPTIONS = [
  // Governance
  { value: "governance_add_member", label: "Add Governance Member" },
  { value: "governance_remove_member", label: "Remove Governance Member" },
  { value: "governance_set_threshold", label: "Set Governance Threshold" },
  { value: "governance_set_timeout", label: "Set Governance Timeout" },
  // Vault Deployment
  { value: "vault_deployment", label: "Deploy Vault" },
  { value: "yield_epoch_deployment", label: "Deploy YieldEpoch" },
  // Vault Operations
  { value: "vault_pause", label: "Pause Vault" },
  { value: "vault_unpause", label: "Unpause Vault" },
  { value: "vault_update_limits", label: "Update Vault Limits" },
  { value: "vault_update_backend", label: "Update Vault Backend" },
  {
    value: "vault_update_far_beneficiaries",
    label: "Update FAR Beneficiaries",
  },
  // Processor
  {
    value: "processor_deployment_request",
    label: "Request Processor Deployment",
  },
  // Utility Onboarding
  {
    value: "utility_create_provider_request",
    label: "Utility: Create Provider",
  },
  { value: "utility_create_user_request", label: "Utility: Create User" },
  { value: "utility_setup", label: "Utility: Setup" },
  {
    value: "utility_accept_holder_service_request",
    label: "Utility: Accept Holder Service",
  },
  // Credential Actions
  { value: "credential_offer_free", label: "Credential: Offer Free" },
  { value: "credential_accept_free", label: "Credential: Accept Free" },
  // DevNet
  { value: "dev_net_feature_app", label: "DevNet: Feature App" },
] as const;

type ActionTypeKey = (typeof ACTION_TYPE_OPTIONS)[number]["value"];

// Format an ActionType for display
const formatActionType = (action: ActionType): string => {
  const label =
    ACTION_TYPE_OPTIONS.find((opt) => opt.value === action.type)?.label ||
    action.type;
  return label;
};

interface GovernanceSectionProps {
  partyId: string;
  rulesContractId?: string;
  memberPartyId?: string;
  defaultOperatorParty?: string;
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
  featured_app_right_cid: DEVNET_FEATURED_APP_RIGHT_CID,
  beneficiaries: [],
};
const defaultVaultRulesCid = DEVNET_VAULT_RULES_CID;
const defaultVaultBackendSignatory = DEVNET_VAULT_BACKEND_SIGNATORY;

export const GovernanceSection = ({
  partyId,
  rulesContractId: initialRulesContractId,
  memberPartyId,
  defaultOperatorParty,
}: GovernanceSectionProps) => {
  const [expanded, setExpanded] = useState(true);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [data, setData] = useState<GovernanceResponse | null>(null);
  const [actionLoading, setActionLoading] = useState<string | null>(null);
  const [executeDialogAction, setExecuteDialogAction] =
    useState<GovernanceAction | null>(null);
  const [executeError, setExecuteError] = useState<string | null>(null);
  const [rulesContractId, setRulesContractId] = useState(
    initialRulesContractId || "",
  );

  // Action form state
  const [showNewActionForm, setShowNewActionForm] = useState(false);
  const [selectedActionType, setSelectedActionType] = useState<ActionTypeKey>(
    "governance_add_member",
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
  const [vaultCid, setVaultCid] = useState(DEVNET_VAULT_RULES_CID);
  const [vaultId, setVaultId] = useState("");
  const [vaultRulesCid, setVaultRulesCid] = useState(defaultVaultRulesCid);
  const [vaultProcessorRulesCid, setVaultProcessorRulesCid] = useState(
    DEVNET_VAULT_PROCESSOR_RULES_CID,
  );

  // New fields for additional action types
  const [operatorParty, setOperatorParty] = useState(defaultOperatorParty || DEVNET_OPERATOR);
  const [providerServiceCid, setProviderServiceCid] = useState("");
  const [userServiceCid, setUserServiceCid] = useState("");
  const [amuletRulesCid, setAmuletRulesCid] = useState(DEVNET_AMULET_RULES_CID);
  const [allocationFactoryCid, setAllocationFactoryCid] = useState(
    DEVNET_ALLOCATION_FACTORY_CID,
  );
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
  const [registrarServices, setRegistrarServices] = useState<
    RegistrarServiceInfo[]
  >([]);
  const [servicesLoading, setServicesLoading] = useState(false);
  const [registrarServicesLoading, setRegistrarServicesLoading] =
    useState(false);

  // Update rulesContractId when prop changes
  useEffect(() => {
    if (initialRulesContractId) {
      setRulesContractId(initialRulesContractId);
    }
  }, [initialRulesContractId]);

  const fetchGovernance = useCallback(async () => {
    try {
      const res = await fetch(
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
      const res = await fetch(
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
      selectedActionType === "yield_epoch_deployment"
    ) {
      fetchVaults();
    }
  }, [selectedActionType, fetchVaults]);

  // Fetch available services from ACS
  const fetchServices = useCallback(async () => {
    setServicesLoading(true);
    try {
      const [providerRes, userRes] = await Promise.all([
        fetch(
          `${API_BASE}/services/provider?party_id=${encodeURIComponent(partyId)}`,
        ),
        fetch(
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

  // Fetch services when action type is utility_setup
  useEffect(() => {
    if (selectedActionType === "utility_setup") {
      fetchServices();
    }
  }, [selectedActionType, fetchServices]);

  // Fetch registrar services from ACS
  const fetchRegistrarServices = useCallback(async () => {
    setRegistrarServicesLoading(true);
    try {
      const res = await fetch(
        `${API_BASE}/services/registrar?party_id=${encodeURIComponent(partyId)}`,
      );
      if (res.ok) {
        const response: RegistrarServicesResponse = await res.json();
        setRegistrarServices(response.services);
        // Auto-select first registrar service
        if (response.services.length > 0) {
          setRegistrarServiceCid(response.services[0].contract_id);
        }
      }
    } catch (e) {
      console.error("Failed to fetch registrar services:", e);
    } finally {
      setRegistrarServicesLoading(false);
    }
  }, [partyId]);

  // Fetch registrar services when action type is vault_deployment
  useEffect(() => {
    if (selectedActionType === "vault_deployment") {
      fetchRegistrarServices();
    }
  }, [selectedActionType, fetchRegistrarServices]);

  const handleConfirm = async (action: GovernanceAction) => {
    if (!rulesContractId) {
      setError("Please enter the VaultGovernanceRules contract ID");
      return;
    }

    setActionLoading(action.action_hash);
    setError(null);

    try {
      const request: ConfirmActionRequest = {
        party_id: partyId,
        rules_contract_id: rulesContractId,
        action: action.action,
      };

      const res = await fetch(`${API_BASE}/governance/confirm`, {
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
      setExecuteError("Please enter the VaultGovernanceRules contract ID");
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
      };

      const res = await fetch(`${API_BASE}/governance/execute`, {
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

  const handleRevoke = async (action: GovernanceAction, confirmationCid: string) => {
    if (!rulesContractId) {
      setError("Please enter the VaultGovernanceRules contract ID");
      return;
    }

    setActionLoading(action.action_hash);
    setError(null);

    try {
      const request: ExpireConfirmationRequest = {
        party_id: partyId,
        rules_contract_id: rulesContractId,
        confirmation_cid: confirmationCid,
      };

      const res = await fetch(`${API_BASE}/governance/expire`, {
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
      setError(e instanceof Error ? e.message : "Failed to revoke confirmation");
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
    return action.confirmations.find((c) => c.confirming_party === myMemberPartyId);
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
          allocation_factory_cid: allocationFactoryCid,
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

  const handleSubmitAction = async () => {
    if (!rulesContractId) {
      setError("Please enter the VaultGovernanceRules contract ID");
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
      };

      const res = await fetch(`${API_BASE}/governance/confirm`, {
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
          <TextField
            label="Vault Contract ID"
            value={vaultId}
            onChange={(e) => setVaultId(e.target.value)}
            size="small"
            fullWidth
          />
        );
      case "vault_update_limits":
        return (
          <>
            <TextField
              label="Vault Contract ID"
              value={vaultId}
              onChange={(e) => setVaultId(e.target.value)}
              size="small"
              fullWidth
              sx={{ mb: 2 }}
            />
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
            <TextField
              label="Vault Contract ID"
              value={vaultId}
              onChange={(e) => setVaultId(e.target.value)}
              size="small"
              fullWidth
              sx={{ mb: 2 }}
            />
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
            <TextField
              label="Vault Rules Contract ID"
              value={vaultRulesCid}
              onChange={(e) => setVaultRulesCid(e.target.value)}
              size="small"
              fullWidth
              sx={{ mb: 2 }}
              required
            />
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
            <Typography variant="caption" color="text.secondary">
              FAR Config (Optional)
            </Typography>
            <TextField
              label="Featured App Right Contract ID"
              value={vaultFarConfig.featured_app_right_cid}
              onChange={(e) =>
                setVaultFarConfig({
                  ...vaultFarConfig,
                  featured_app_right_cid: e.target.value,
                })
              }
              size="small"
              fullWidth
              sx={{ mb: 1 }}
            />
            <Typography variant="caption" color="text.secondary" sx={{ mt: 1 }}>
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
              sx={{ mb: 2 }}
            >
              Add Beneficiary
            </Button>
            <TextField
              label="Allocation Factory Contract ID"
              value={allocationFactoryCid}
              onChange={(e) => setAllocationFactoryCid(e.target.value)}
              size="small"
              fullWidth
              sx={{ mb: 2 }}
              required
              helperText="From SetupUtility result"
            />
            <FormControl fullWidth size="small" required>
              <InputLabel>Registrar Service</InputLabel>
              <Select
                value={registrarServiceCid}
                label="Registrar Service"
                onChange={(e) => setRegistrarServiceCid(e.target.value)}
              >
                {registrarServicesLoading ? (
                  <MenuItem disabled>Loading registrar services...</MenuItem>
                ) : registrarServices.length > 0 ? (
                  registrarServices.map((service) => (
                    <MenuItem
                      key={service.contract_id}
                      value={service.contract_id}
                    >
                      {service.contract_id.slice(0, 16)}...
                    </MenuItem>
                  ))
                ) : (
                  <MenuItem disabled>No registrar services found</MenuItem>
                )}
              </Select>
            </FormControl>
          </>
        );
      case "yield_epoch_deployment":
        return (
          <>
            <TextField
              label="Vault Rules Contract ID"
              value={vaultRulesCid}
              onChange={(e) => setVaultRulesCid(e.target.value)}
              size="small"
              fullWidth
              sx={{ mb: 2 }}
              required
            />
            <FormControl fullWidth size="small" sx={{ mb: 2 }}>
              <InputLabel>Vault</InputLabel>
              <Select
                value={vaultCid}
                label="Vault"
                onChange={(e) => setVaultCid(e.target.value)}
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
            <TextField
              label="Vault Contract ID"
              value={vaultId}
              onChange={(e) => setVaultId(e.target.value)}
              size="small"
              fullWidth
              sx={{ mb: 2 }}
            />
            <Typography variant="caption" color="text.secondary">
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
            <TextField
              label="Allocation Factory Contract ID"
              value={allocationFactoryCid}
              onChange={(e) => setAllocationFactoryCid(e.target.value)}
              size="small"
              fullWidth
              sx={{ mb: 2 }}
            />
            <Typography variant="caption" color="text.secondary">
              FAR Config (Optional)
            </Typography>
            <TextField
              label="Featured App Right Contract ID"
              value={vaultFarConfig.featured_app_right_cid}
              onChange={(e) =>
                setVaultFarConfig({
                  ...vaultFarConfig,
                  featured_app_right_cid: e.target.value,
                })
              }
              size="small"
              fullWidth
              sx={{ mb: 1 }}
            />
            <Typography variant="caption" color="text.secondary" sx={{ mt: 1 }}>
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
              sx={{ mb: 2 }}
            >
              Add Beneficiary
            </Button>
            <Typography variant="caption" color="text.secondary">
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
                          sx={{ fontFamily: "monospace" }}
                        >
                          {vault.contract_id.slice(0, 20)}...
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
            <FormControl fullWidth size="small" sx={{ mb: 2 }}>
              <InputLabel>Provider Service</InputLabel>
              <Select
                value={providerServiceCid}
                label="Provider Service"
                onChange={(e) => setProviderServiceCid(e.target.value)}
              >
                {servicesLoading ? (
                  <MenuItem disabled>Loading services...</MenuItem>
                ) : providerServices.length > 0 ? (
                  providerServices.map((svc) => (
                    <MenuItem key={svc.contract_id} value={svc.contract_id}>
                      Provider: {svc.provider.split("::")[0]}
                    </MenuItem>
                  ))
                ) : (
                  <MenuItem disabled>No provider services found</MenuItem>
                )}
              </Select>
            </FormControl>
            <FormControl fullWidth size="small">
              <InputLabel>User Service</InputLabel>
              <Select
                value={userServiceCid}
                label="User Service"
                onChange={(e) => setUserServiceCid(e.target.value)}
              >
                {servicesLoading ? (
                  <MenuItem disabled>Loading services...</MenuItem>
                ) : userServices.length > 0 ? (
                  userServices.map((svc) => (
                    <MenuItem key={svc.contract_id} value={svc.contract_id}>
                      User: {svc.user.split("::")[0]}
                    </MenuItem>
                  ))
                ) : (
                  <MenuItem disabled>No user services found</MenuItem>
                )}
              </Select>
            </FormControl>
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
            <TextField
              label="Provider Service Contract ID"
              value={providerServiceCid}
              onChange={(e) => setProviderServiceCid(e.target.value)}
              size="small"
              fullWidth
              sx={{ mb: 2 }}
            />
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
            <TextField
              label="User Service Contract ID"
              value={userServiceCid}
              onChange={(e) => setUserServiceCid(e.target.value)}
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
            <TextField
              label="User Service Contract ID"
              value={userServiceCid}
              onChange={(e) => setUserServiceCid(e.target.value)}
              size="small"
              fullWidth
              sx={{ mb: 2 }}
            />
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
          <TextField
            label="Amulet Rules Contract ID"
            value={amuletRulesCid}
            onChange={(e) => setAmuletRulesCid(e.target.value)}
            size="small"
            fullWidth
          />
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
          <TextField
            label="VaultGovernanceRules Contract ID"
            value={rulesContractId}
            onChange={(e) => setRulesContractId(e.target.value)}
            size="small"
            fullWidth
            placeholder="Enter contract ID to enable confirm/execute"
            disabled={!ADMIN_ACCESS}
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
                >
                  {ACTION_TYPE_OPTIONS.map((opt) => (
                    <MenuItem key={opt.value} value={opt.value}>
                      {opt.label}
                    </MenuItem>
                  ))}
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
          <Box sx={{ overflowX: "auto" }}>
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
                {data.actions.map((action) => (
                  <TableRow key={action.action_hash}>
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
                      <Chip
                        label={`${action.confirmation_count} / ${data.threshold}`}
                        size="small"
                        color={action.can_execute ? "success" : "default"}
                      />
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
                              !rulesContractId ||
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
          <Typography variant="body2" color="text.secondary" sx={{ py: 2 }}>
            No governance actions found for this party.
          </Typography>
        )}
      </Collapse>

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
      />
    </Box>
  );
};
