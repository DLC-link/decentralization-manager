import { useState, useMemo } from "react";
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
  Tooltip,
} from "@mui/material";
import DeleteIcon from "@mui/icons-material/Delete";
import AddIcon from "@mui/icons-material/Add";
import {
  DEVNET_VAULT_RULES,
  DEVNET_VAULT_PROCESSOR_RULES,
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
  credential_offer_free: "Offer Free Credential",
  credential_accept_free: "Accept Free Credential",
  dev_net_feature_app: "DevNet Feature App",
};

const STATIC_BLOB_MAP: Record<string, string> = Object.fromEntries(
  [DEVNET_VAULT_RULES, DEVNET_VAULT_PROCESSOR_RULES].map(
    (c) => [c.contract_id, c.blob],
  ),
);

const formatActionType = (action: ActionType): string =>
  ACTION_TYPE_LABELS[action.type] || action.type;

// Get the contract IDs that need blobs for a given action
const getRequiredContractIds = (action: ActionType): string[] => {
  switch (action.type) {
    case "dev_net_feature_app":
      return [action.amulet_rules_cid];
    case "vault_deployment": {
      const ids = [action.vault_rules_cid, action.allocation_factory_cid];
      if (action.vault_far_config) {
        ids.push(action.vault_far_config.featured_app_right_cid);
      }
      return ids;
    }
    case "processor_deployment_request": {
      const ids = [action.vault_processor_rules_cid, action.allocation_factory_cid];
      if (action.processor_far_config) {
        ids.push(action.processor_far_config.featured_app_right_cid);
      }
      return ids;
    }
    default:
      return [];
  }
};

/// Heuristic for "this error came from the Canton ledger" — those messages
/// carry Canton's gRPC error envelope (`code: '...', message: "..."`) and
/// usually a `DAML_*` error category from the Daml interpreter. Operators
/// can't fix these from the UI, so we tell them to forward to the BitSafe
/// team.
const isChainError = (msg: string): boolean =>
  msg.includes("DAML_") ||
  msg.includes("code: ") ||
  msg.includes("INVALID_GIVEN_CURRENT_SYSTEM_STATE") ||
  msg.includes("CONTRACT_NOT_FOUND");

interface ExecuteDialogProps {
  open: boolean;
  onClose: () => void;
  onExecute: (disclosedContracts: DisclosedContractInput[]) => void;
  action: GovernanceAction | null;
  loading: boolean;
  error: string | null;
  /// Optional dismiss callback — when provided, the inline error Alert
  /// gets an X button that calls this to clear the message.
  onErrorDismiss?: () => void;
  blobMap?: Record<string, string>;
}

interface ExecuteFormProps {
  action: GovernanceAction;
  fullBlobMap: Record<string, string>;
  loading: boolean;
  error: string | null;
  onErrorDismiss?: () => void;
  onClose: () => void;
  onExecute: (disclosedContracts: DisclosedContractInput[]) => void;
}

// Inner form. The parent re-keys it on action.action_hash so it remounts —
// state is initialized lazily from the action prop instead of via an effect.
const ExecuteForm = ({
  action,
  fullBlobMap,
  loading,
  error,
  onErrorDismiss,
  onClose,
  onExecute,
}: ExecuteFormProps) => {
  const [disclosedContracts, setDisclosedContracts] = useState<
    DisclosedContractInput[]
  >(() =>
    getRequiredContractIds(action.action).map((cid) => ({
      contract_id: cid,
      blob: fullBlobMap[cid] || "",
    })),
  );

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

  return (
    <>
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
                      <Tooltip title="Remove disclosed contract">
                        <span>
                          <IconButton
                            size="small"
                            onClick={() => handleRemove(index)}
                            disabled={loading}
                          >
                            <DeleteIcon fontSize="small" />
                          </IconButton>
                        </span>
                      </Tooltip>
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

          {error && (
            <Alert
              severity="error"
              onClose={onErrorDismiss ? () => onErrorDismiss() : undefined}
            >
              <Box sx={{ display: "flex", flexDirection: "column", gap: 1 }}>
                <span>{error}</span>
                {isChainError(error) && (
                  <Typography variant="caption">
                    This is a chain error message — please forward the issue
                    to the BitSafe team.
                  </Typography>
                )}
              </Box>
            </Alert>
          )}
        </Box>
      </DialogContent>
      <DialogActions>
        <Button onClick={onClose} disabled={loading}>
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
    </>
  );
};

export const ExecuteDialog = ({
  open,
  onClose,
  onExecute,
  action,
  loading,
  error,
  onErrorDismiss,
  blobMap = {},
}: ExecuteDialogProps) => {
  const fullBlobMap = useMemo(
    () => ({ ...STATIC_BLOB_MAP, ...blobMap }),
    [blobMap],
  );

  return (
    <Dialog open={open} onClose={onClose} maxWidth="md" fullWidth>
      {action && (
        <ExecuteForm
          key={action.action_hash}
          action={action}
          fullBlobMap={fullBlobMap}
          loading={loading}
          error={error}
          onErrorDismiss={onErrorDismiss}
          onClose={onClose}
          onExecute={onExecute}
        />
      )}
    </Dialog>
  );
};
