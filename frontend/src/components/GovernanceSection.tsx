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
} from "@mui/material";
import ExpandMoreIcon from "@mui/icons-material/ExpandMore";
import ExpandLessIcon from "@mui/icons-material/ExpandLess";
import CheckCircleIcon from "@mui/icons-material/CheckCircle";
import PlayArrowIcon from "@mui/icons-material/PlayArrow";
import { API_BASE, MAINNET_DEMO } from "../constants";
import type {
  GovernanceResponse,
  GovernanceAction,
  ConfirmActionRequest,
  ExecuteActionRequest,
} from "../types";

interface GovernanceSectionProps {
  partyId: string;
}

export const GovernanceSection = ({ partyId }: GovernanceSectionProps) => {
  const [expanded, setExpanded] = useState(true);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [data, setData] = useState<GovernanceResponse | null>(null);
  const [actionLoading, setActionLoading] = useState<string | null>(null);
  const [rulesContractId, setRulesContractId] = useState("");

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
