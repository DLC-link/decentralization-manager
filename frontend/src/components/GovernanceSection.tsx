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
} from "@mui/material";
import ExpandMoreIcon from "@mui/icons-material/ExpandMore";
import ExpandLessIcon from "@mui/icons-material/ExpandLess";
import CheckCircleIcon from "@mui/icons-material/CheckCircle";
import PlayArrowIcon from "@mui/icons-material/PlayArrow";
import AddIcon from "@mui/icons-material/Add";
import { API_BASE, MAINNET_DEMO } from "../constants";
import type {
  GovernanceResponse,
  GovernanceAction,
  ConfirmActionRequest,
  ExecuteActionRequest,
  ActionType,
  ConfirmActionRequestV2,
  InstrumentId,
  VaultLimits,
  FarConfig,
} from "../types";

// Action type labels for display
const ACTION_TYPE_OPTIONS = [
  { value: "governance_add_member", label: "Add Governance Member" },
  { value: "governance_remove_member", label: "Remove Governance Member" },
  { value: "governance_set_threshold", label: "Set Governance Threshold" },
  { value: "governance_set_timeout", label: "Set Governance Timeout" },
  { value: "vault_deployment", label: "Vault Deployment" },
  { value: "yield_epoch_deployment", label: "Yield Epoch Deployment" },
  { value: "vault_pause", label: "Pause Vault" },
  { value: "vault_unpause", label: "Unpause Vault" },
  { value: "vault_update_limits", label: "Update Vault Limits" },
  { value: "vault_update_backend", label: "Update Vault Backend" },
] as const;

type ActionTypeKey = (typeof ACTION_TYPE_OPTIONS)[number]["value"];

interface GovernanceSectionProps {
  partyId: string;
}

// Default values for action form
const defaultInstrumentId: InstrumentId = { issuer: "", symbol: "" };
const defaultVaultLimits: VaultLimits = {
  max_total_deposit: "0",
  min_deposit_amount: "0",
  min_withdrawal_amount: "0",
};
const defaultFarConfig: FarConfig = {
  featured_app_right_cid: "",
  beneficiaries: [],
};

export const GovernanceSection = ({ partyId }: GovernanceSectionProps) => {
  const [expanded, setExpanded] = useState(true);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [data, setData] = useState<GovernanceResponse | null>(null);
  const [actionLoading, setActionLoading] = useState<string | null>(null);
  const [rulesContractId, setRulesContractId] = useState("");

  // V2 action form state
  const [showNewActionForm, setShowNewActionForm] = useState(false);
  const [selectedActionType, setSelectedActionType] =
    useState<ActionTypeKey>("governance_add_member");
  const [v2Loading, setV2Loading] = useState(false);

  // Form fields for various action types
  const [memberParty, setMemberParty] = useState("");
  const [newThreshold, setNewThreshold] = useState(2);
  const [timeoutMicroseconds, setTimeoutMicroseconds] = useState(3600000000);
  const [vaultName, setVaultName] = useState("");
  const [shareSymbol, setShareSymbol] = useState("");
  const [assetInstrumentId, setAssetInstrumentId] =
    useState<InstrumentId>(defaultInstrumentId);
  const [vaultLimits, setVaultLimits] =
    useState<VaultLimits>(defaultVaultLimits);
  const [vaultManager, setVaultManager] = useState("");
  const [vaultBackendSignatory, setVaultBackendSignatory] = useState("");
  const [vaultFarConfig, setVaultFarConfig] =
    useState<FarConfig>(defaultFarConfig);
  const [vaultCid, setVaultCid] = useState("");
  const [vaultId, setVaultId] = useState("");

  const fetchGovernance = useCallback(async () => {
    try {
      const res = await fetch(
        `${API_BASE}/governance/confirmations?party_id=${encodeURIComponent(partyId)}`
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
      setError(e instanceof Error ? e.message : "Failed to fetch governance data");
    } finally {
      setLoading(false);
    }
  }, [partyId]);

  useEffect(() => {
    fetchGovernance();
    const interval = setInterval(fetchGovernance, 10000); // Poll every 10 seconds
    return () => clearInterval(interval);
  }, [fetchGovernance]);

  const handleConfirm = async (action: GovernanceAction) => {
    if (!rulesContractId) {
      setError("Please enter the VaultGovernanceRules contract ID");
      return;
    }

    setActionLoading(action.action_id);
    setError(null);

    try {
      const request: ConfirmActionRequest = {
        party_id: partyId,
        action_id: action.action_id,
        rules_contract_id: rulesContractId,
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
      setError(e instanceof Error ? e.message : "Failed to submit confirmation");
    } finally {
      setActionLoading(null);
    }
  };

  const handleExecute = async (action: GovernanceAction) => {
    if (!rulesContractId) {
      setError("Please enter the VaultGovernanceRules contract ID");
      return;
    }

    setActionLoading(action.action_id);
    setError(null);

    try {
      const request: ExecuteActionRequest = {
        party_id: partyId,
        action_id: action.action_id,
        rules_contract_id: rulesContractId,
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

      // Refresh data
      await fetchGovernance();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to execute action");
    } finally {
      setActionLoading(null);
    }
  };

  // Build ActionType from form state
  const buildActionFromForm = (): ActionType | null => {
    switch (selectedActionType) {
      case "governance_add_member":
        return { type: "governance_add_member", member: memberParty, new_threshold: newThreshold };
      case "governance_remove_member":
        return { type: "governance_remove_member", member: memberParty, new_threshold: newThreshold };
      case "governance_set_threshold":
        return { type: "governance_set_threshold", new_threshold: newThreshold };
      case "governance_set_timeout":
        return { type: "governance_set_timeout", new_timeout_microseconds: timeoutMicroseconds };
      case "vault_deployment":
        return {
          type: "vault_deployment",
          vault_name: vaultName,
          share_symbol: shareSymbol,
          asset_instrument_id: assetInstrumentId,
          limits: vaultLimits,
          vault_manager: vaultManager,
          vault_backend_signatory: vaultBackendSignatory,
          vault_far_config: vaultFarConfig,
        };
      case "yield_epoch_deployment":
        return {
          type: "yield_epoch_deployment",
          vault_cid: vaultCid,
          vault_manager: vaultManager,
          asset_instrument_id: assetInstrumentId,
          vault_backend_signatory: vaultBackendSignatory,
        };
      case "vault_pause":
        return { type: "vault_pause", vault_id: vaultId };
      case "vault_unpause":
        return { type: "vault_unpause", vault_id: vaultId };
      case "vault_update_limits":
        return { type: "vault_update_limits", vault_id: vaultId, new_limits: vaultLimits };
      case "vault_update_backend":
        return { type: "vault_update_backend", vault_id: vaultId, new_backend_signatory: vaultBackendSignatory };
      default:
        return null;
    }
  };

  const handleSubmitV2Action = async () => {
    if (!rulesContractId) {
      setError("Please enter the VaultGovernanceRules contract ID");
      return;
    }

    const action = buildActionFromForm();
    if (!action) {
      setError("Invalid action type");
      return;
    }

    setV2Loading(true);
    setError(null);

    try {
      const request: ConfirmActionRequestV2 = {
        party_id: partyId,
        rules_contract_id: rulesContractId,
        action,
      };

      const res = await fetch(`${API_BASE}/governance/v2/confirm`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(request),
      });

      if (!res.ok) {
        const errData = await res.json().catch(() => ({}));
        throw new Error(errData.error || "Failed to submit V2 confirmation");
      }

      // Reset form and refresh data
      setShowNewActionForm(false);
      await fetchGovernance();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to submit V2 confirmation");
    } finally {
      setV2Loading(false);
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
            onChange={(e) => setTimeoutMicroseconds(parseInt(e.target.value) || 0)}
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
            <TextField
              label="Max Total Deposit"
              value={vaultLimits.max_total_deposit}
              onChange={(e) =>
                setVaultLimits({ ...vaultLimits, max_total_deposit: e.target.value })
              }
              size="small"
              fullWidth
              sx={{ mb: 2 }}
            />
            <TextField
              label="Min Deposit Amount"
              value={vaultLimits.min_deposit_amount}
              onChange={(e) =>
                setVaultLimits({ ...vaultLimits, min_deposit_amount: e.target.value })
              }
              size="small"
              fullWidth
              sx={{ mb: 2 }}
            />
            <TextField
              label="Min Withdrawal Amount"
              value={vaultLimits.min_withdrawal_amount}
              onChange={(e) =>
                setVaultLimits({ ...vaultLimits, min_withdrawal_amount: e.target.value })
              }
              size="small"
              fullWidth
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
              label="Issuer Party"
              value={assetInstrumentId.issuer}
              onChange={(e) =>
                setAssetInstrumentId({ ...assetInstrumentId, issuer: e.target.value })
              }
              size="small"
              fullWidth
              sx={{ mb: 1 }}
            />
            <TextField
              label="Symbol"
              value={assetInstrumentId.symbol}
              onChange={(e) =>
                setAssetInstrumentId({ ...assetInstrumentId, symbol: e.target.value })
              }
              size="small"
              fullWidth
              sx={{ mb: 2 }}
            />
            <Typography variant="caption" color="text.secondary">
              Vault Limits
            </Typography>
            <TextField
              label="Max Total Deposit"
              value={vaultLimits.max_total_deposit}
              onChange={(e) =>
                setVaultLimits({ ...vaultLimits, max_total_deposit: e.target.value })
              }
              size="small"
              fullWidth
              sx={{ mb: 1 }}
            />
            <TextField
              label="Min Deposit Amount"
              value={vaultLimits.min_deposit_amount}
              onChange={(e) =>
                setVaultLimits({ ...vaultLimits, min_deposit_amount: e.target.value })
              }
              size="small"
              fullWidth
              sx={{ mb: 1 }}
            />
            <TextField
              label="Min Withdrawal Amount"
              value={vaultLimits.min_withdrawal_amount}
              onChange={(e) =>
                setVaultLimits({ ...vaultLimits, min_withdrawal_amount: e.target.value })
              }
              size="small"
              fullWidth
              sx={{ mb: 2 }}
            />
            <TextField
              label="Vault Manager Party"
              value={vaultManager}
              onChange={(e) => setVaultManager(e.target.value)}
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
              sx={{ mb: 2 }}
            />
            <TextField
              label="Featured App Right Contract ID"
              value={vaultFarConfig.featured_app_right_cid}
              onChange={(e) =>
                setVaultFarConfig({ ...vaultFarConfig, featured_app_right_cid: e.target.value })
              }
              size="small"
              fullWidth
            />
          </>
        );
      case "yield_epoch_deployment":
        return (
          <>
            <TextField
              label="Vault Contract ID"
              value={vaultCid}
              onChange={(e) => setVaultCid(e.target.value)}
              size="small"
              fullWidth
              sx={{ mb: 2 }}
            />
            <TextField
              label="Vault Manager Party"
              value={vaultManager}
              onChange={(e) => setVaultManager(e.target.value)}
              size="small"
              fullWidth
              sx={{ mb: 2 }}
            />
            <Typography variant="caption" color="text.secondary">
              Asset Instrument ID
            </Typography>
            <TextField
              label="Issuer Party"
              value={assetInstrumentId.issuer}
              onChange={(e) =>
                setAssetInstrumentId({ ...assetInstrumentId, issuer: e.target.value })
              }
              size="small"
              fullWidth
              sx={{ mb: 1 }}
            />
            <TextField
              label="Symbol"
              value={assetInstrumentId.symbol}
              onChange={(e) =>
                setAssetInstrumentId({ ...assetInstrumentId, symbol: e.target.value })
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
            disabled={MAINNET_DEMO}
          />
        </Box>

        {/* V2 New Action Form */}
        <Box sx={{ mb: 2 }}>
          <Button
            size="small"
            variant="outlined"
            startIcon={showNewActionForm ? <ExpandLessIcon /> : <AddIcon />}
            onClick={() => setShowNewActionForm(!showNewActionForm)}
            disabled={MAINNET_DEMO || !rulesContractId}
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
                Create New Governance Action (V2)
              </Typography>

              <FormControl fullWidth size="small" sx={{ mb: 2 }}>
                <InputLabel>Action Type</InputLabel>
                <Select
                  value={selectedActionType}
                  label="Action Type"
                  onChange={(e) => setSelectedActionType(e.target.value as ActionTypeKey)}
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
                  onClick={handleSubmitV2Action}
                  disabled={v2Loading || !rulesContractId}
                  startIcon={v2Loading ? <CircularProgress size={16} /> : <CheckCircleIcon />}
                >
                  Submit Confirmation
                </Button>
                <Button variant="outlined" onClick={() => setShowNewActionForm(false)}>
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
                  <TableRow key={action.action_id}>
                    <TableCell sx={{ py: 1 }}>
                      <Typography variant="body2" sx={{ fontFamily: "monospace" }}>
                        {action.action_id.length > 50
                          ? `${action.action_id.substring(0, 50)}...`
                          : action.action_id}
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
                      <Box sx={{ display: "flex", gap: 1, justifyContent: "flex-end" }}>
                        <Button
                          size="small"
                          variant="outlined"
                          startIcon={
                            actionLoading === action.action_id ? (
                              <CircularProgress size={16} />
                            ) : (
                              <CheckCircleIcon />
                            )
                          }
                          onClick={() => handleConfirm(action)}
                          disabled={
                            MAINNET_DEMO ||
                            !rulesContractId ||
                            actionLoading === action.action_id
                          }
                        >
                          Confirm
                        </Button>
                        {action.can_execute && (
                          <Button
                            size="small"
                            variant="contained"
                            color="success"
                            startIcon={
                              actionLoading === action.action_id ? (
                                <CircularProgress size={16} color="inherit" />
                              ) : (
                                <PlayArrowIcon />
                              )
                            }
                            onClick={() => handleExecute(action)}
                            disabled={
                              MAINNET_DEMO ||
                              !rulesContractId ||
                              actionLoading === action.action_id
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
    </Box>
  );
};
