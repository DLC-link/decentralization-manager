import { useState, useEffect } from "react";
import {
  Dialog,
  DialogTitle,
  DialogContent,
  DialogActions,
  Button,
  TextField,
  Typography,
  CircularProgress,
  Alert,
  Box,
  IconButton,
} from "@mui/material";
import DeleteIcon from "@mui/icons-material/Delete";
import AddIcon from "@mui/icons-material/Add";
import {
  DEVNET_AMULET_RULES_BLOB,
  DEVNET_VAULT_RULES_BLOB,
  DEVNET_VAULT_PROCESSOR_RULES_BLOB,
  DEVNET_ALLOCATION_FACTORY_BLOB,
  DEVNET_FEATURED_APP_RIGHT_CID,
  DEVNET_FEATURED_APP_RIGHT_BLOB,
} from "../constants";
import type {
  GovernanceAction,
  DisclosedContractInput,
  ActionType,
} from "../types";

const ACTION_TYPE_LABELS: Record<string, string> = {
  governance_add_member: "Add Governance Member",
  governance_remove_member: "Remove Governance Member",
  governance_set_threshold: "Set Governance Threshold",
  governance_set_timeout: "Set Governance Timeout",
  vault_deployment: "Deploy Vault",
  yield_epoch_deployment: "Deploy YieldEpoch",
  vault_pause: "Pause Vault",
  vault_unpause: "Unpause Vault",
  vault_update_limits: "Update Vault Limits",
  vault_update_backend: "Update Vault Backend",
  vault_update_far_beneficiaries: "Update FAR Beneficiaries",
  processor_deployment_request: "Deploy Processor",
  utility_create_provider_request: "Create Provider Request",
  utility_create_user_request: "Create User Request",
  utility_setup: "Utility Setup",
  utility_accept_holder_service_request: "Accept Holder Service Request",
  utility_create_transfer_rule: "Create Transfer Rule",
  credential_offer_free: "Offer Free Credential",
  credential_accept_free: "Accept Free Credential",
  dev_net_feature_app: "DevNet Feature App",
};

const formatActionType = (action: ActionType): string =>
  ACTION_TYPE_LABELS[action.type] || action.type;

const getDefaultDisclosedContracts = (
  action: ActionType,
): DisclosedContractInput[] => {
  switch (action.type) {
    case "dev_net_feature_app":
      return [
        {
          contract_id: action.amulet_rules_cid,
          blob: DEVNET_AMULET_RULES_BLOB,
        },
      ];
    case "vault_deployment":
      return [
        {
          contract_id: action.vault_rules_cid,
          blob: DEVNET_VAULT_RULES_BLOB,
        },
      ];
    case "processor_deployment_request":
      return [
        {
          contract_id: action.vault_processor_rules_cid,
          blob: DEVNET_VAULT_PROCESSOR_RULES_BLOB,
        },
        {
          contract_id: action.allocation_factory_cid,
          blob: DEVNET_ALLOCATION_FACTORY_BLOB,
        },
        {
          contract_id: DEVNET_FEATURED_APP_RIGHT_CID,
          blob: DEVNET_FEATURED_APP_RIGHT_BLOB,
        },
      ];
    default:
      return [];
  }
};

interface ExecuteDialogProps {
  open: boolean;
  onClose: () => void;
  onExecute: (disclosedContracts: DisclosedContractInput[]) => void;
  action: GovernanceAction | null;
  loading: boolean;
  error: string | null;
}

export const ExecuteDialog = ({
  open,
  onClose,
  onExecute,
  action,
  loading,
  error,
}: ExecuteDialogProps) => {
  const [disclosedContracts, setDisclosedContracts] = useState<
    DisclosedContractInput[]
  >([]);

  useEffect(() => {
    if (open && action) {
      setDisclosedContracts(getDefaultDisclosedContracts(action.action));
    } else {
      setDisclosedContracts([]);
    }
  }, [open, action]);

  const handleAdd = () => {
    setDisclosedContracts((prev) => [
      ...prev,
      { contract_id: "", blob: "" },
    ]);
  };

  const handleRemove = (index: number) => {
    setDisclosedContracts((prev) => prev.filter((_, i) => i !== index));
  };

  const handleChange = (
    index: number,
    field: keyof DisclosedContractInput,
    value: string,
  ) => {
    setDisclosedContracts((prev) =>
      prev.map((dc, i) => (i === index ? { ...dc, [field]: value } : dc)),
    );
  };

  const handleClose = () => {
    if (!loading) {
      onClose();
    }
  };

  if (!action) return null;

  return (
    <Dialog open={open} onClose={handleClose} maxWidth="md" fullWidth>
      <DialogTitle>Execute Governance Action</DialogTitle>
      <DialogContent>
        <Box sx={{ display: "flex", flexDirection: "column", gap: 2, mt: 1 }}>
          <Box>
            <Typography variant="subtitle2" color="text.secondary">
              Action
            </Typography>
            <Typography variant="body1">
              {formatActionType(action.action)}
            </Typography>
          </Box>

          <Box>
            <Typography variant="subtitle2" color="text.secondary">
              Confirmations
            </Typography>
            <Typography variant="body2">
              {action.confirmation_count} confirmation(s)
            </Typography>
          </Box>

          <Box>
            <Box
              sx={{
                display: "flex",
                alignItems: "center",
                justifyContent: "space-between",
                mb: 1,
              }}
            >
              <Typography variant="subtitle2">
                Disclosed Contracts
              </Typography>
              <Button
                size="small"
                startIcon={<AddIcon />}
                onClick={handleAdd}
                disabled={loading}
              >
                Add
              </Button>
            </Box>

            {disclosedContracts.length === 0 ? (
              <Typography variant="body2" color="text.secondary">
                No disclosed contracts. Click "Add" if this action requires
                them.
              </Typography>
            ) : (
              <Box sx={{ display: "flex", flexDirection: "column", gap: 2 }}>
                {disclosedContracts.map((dc, index) => (
                  <Box
                    key={index}
                    sx={{
                      p: 2,
                      border: 1,
                      borderColor: "divider",
                      borderRadius: 1,
                    }}
                  >
                    <Box
                      sx={{
                        display: "flex",
                        alignItems: "center",
                        justifyContent: "space-between",
                        mb: 1,
                      }}
                    >
                      <Typography variant="caption" color="text.secondary">
                        Disclosed Contract #{index + 1}
                      </Typography>
                      <IconButton
                        size="small"
                        onClick={() => handleRemove(index)}
                        disabled={loading}
                      >
                        <DeleteIcon fontSize="small" />
                      </IconButton>
                    </Box>
                    <TextField
                      label="Contract ID"
                      value={dc.contract_id}
                      onChange={(e) =>
                        handleChange(index, "contract_id", e.target.value)
                      }
                      fullWidth
                      size="small"
                      disabled={loading}
                      sx={{ mb: 1 }}
                    />
                    <TextField
                      label="Blob (base64)"
                      value={dc.blob}
                      onChange={(e) =>
                        handleChange(index, "blob", e.target.value)
                      }
                      fullWidth
                      size="small"
                      disabled={loading}
                      multiline
                      minRows={2}
                      maxRows={4}
                    />
                  </Box>
                ))}
              </Box>
            )}
          </Box>

          {error && <Alert severity="error">{error}</Alert>}
        </Box>
      </DialogContent>
      <DialogActions>
        <Button onClick={handleClose} disabled={loading}>
          Cancel
        </Button>
        <Button
          onClick={() => onExecute(disclosedContracts)}
          variant="contained"
          color="success"
          disabled={loading}
        >
          {loading ? <CircularProgress size={20} color="inherit" /> : "Execute"}
        </Button>
      </DialogActions>
    </Dialog>
  );
};
