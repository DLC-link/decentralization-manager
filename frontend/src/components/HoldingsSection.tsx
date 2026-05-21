import { useCallback, useEffect, useState } from "react";
import {
  Alert,
  Box,
  Button,
  Chip,
  CircularProgress,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableRow,
  Typography,
} from "@mui/material";
import RefreshIcon from "@mui/icons-material/Refresh";
import { CopyableText } from "./CopyableText";
import { API_BASE } from "../constants";
import { authenticatedFetch } from "../api";
import { zebraRow } from "../styles";
import type { Holding, HoldingsResponse } from "../types";

interface HoldingsSectionProps {
  partyId: string;
  /// Bumped by the parent's Refresh button. Triggers a fresh fetch.
  refreshNonce?: number;
  /// Reports the loaded holding count to the parent so it can render a badge
  /// in the section header (matches the Audit Trail / Contracts pattern).
  onCountChange?: (count: number) => void;
  /// Reports loading state to the parent so the section's refresh icon can be
  /// disabled while a fetch is in flight.
  onLoadingChange?: (loading: boolean) => void;
}

export const HoldingsSection = ({
  partyId,
  refreshNonce,
  onCountChange,
  onLoadingChange,
}: HoldingsSectionProps) => {
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [holdings, setHoldings] = useState<Holding[]>([]);
  const [loaded, setLoaded] = useState(false);

  const fetchHoldings = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const params = new URLSearchParams({ party_id: partyId });
      const res = await authenticatedFetch(
        `${API_BASE}/holdings?${params}`,
      );
      if (res.ok) {
        const data: HoldingsResponse = await res.json();
        setHoldings(data.holdings);
      } else {
        const errData = await res.json().catch(() => ({}));
        setError(errData.error || "Failed to fetch holdings");
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to fetch holdings");
    } finally {
      setLoading(false);
    }
  }, [partyId]);

  useEffect(() => {
    if (!loaded) {
      setLoaded(true);
      fetchHoldings();
    }
  }, [loaded, fetchHoldings]);

  useEffect(() => {
    if (refreshNonce === undefined || refreshNonce === 0) return;
    fetchHoldings();
  }, [refreshNonce, fetchHoldings]);

  useEffect(() => {
    onCountChange?.(holdings.length);
  }, [holdings.length, onCountChange]);

  useEffect(() => {
    onLoadingChange?.(loading);
  }, [loading, onLoadingChange]);

  if (error) {
    return (
      <Box sx={{ py: 2 }}>
        <Alert
          severity="error"
          sx={{ mb: 2 }}
          onClose={() => setError(null)}
        >
          {error}
        </Alert>
        <Button
          size="small"
          variant="outlined"
          startIcon={<RefreshIcon />}
          onClick={fetchHoldings}
        >
          Retry
        </Button>
      </Box>
    );
  }

  if (loading && holdings.length === 0) {
    return (
      <Box sx={{ display: "flex", alignItems: "center", gap: 1, py: 2, px: 3 }}>
        <CircularProgress size={16} />
        <Typography variant="body2" color="text.secondary">
          Loading holdings…
        </Typography>
      </Box>
    );
  }

  if (holdings.length === 0) {
    return (
      <Typography variant="body2" color="text.secondary" sx={{ py: 2, px: 3 }}>
        This party has no holdings.
      </Typography>
    );
  }

  return (
    <Box sx={{ overflowX: "auto" }}>
      <Table size="small">
        <TableHead>
          <TableRow>
            <TableCell sx={{ py: 1 }}>Asset</TableCell>
            <TableCell sx={{ py: 1 }}>Admin</TableCell>
            <TableCell sx={{ py: 1 }} align="right">
              Amount
            </TableCell>
            <TableCell sx={{ py: 1 }}>Preapproval set up</TableCell>
          </TableRow>
        </TableHead>
        <TableBody>
          {holdings.map((h, idx) => (
            <TableRow
              key={`${h.instrument_admin}::${h.instrument_id}`}
              sx={zebraRow(idx)}
            >
              <TableCell
                sx={{ py: 1, fontFamily: "monospace", fontSize: "0.85rem" }}
              >
                {/* Canton Coin's instrument id on the Splice token-standard
                  * is the literal "Amulet" — display it as "CC" since that's
                  * what users actually call it everywhere else in the UI. */}
                {h.instrument_id === "Amulet" ? "CC" : h.instrument_id}
              </TableCell>
              <TableCell sx={{ py: 1 }}>
                <CopyableText
                  text={h.instrument_admin}
                  truncate={{ start: 8, end: 8 }}
                  variant="caption"
                />
              </TableCell>
              <TableCell
                sx={{
                  py: 1,
                  fontFamily: "monospace",
                  fontSize: "0.85rem",
                }}
                align="right"
              >
                {h.amount}
              </TableCell>
              <TableCell sx={{ py: 1 }}>
                <Chip
                  label={h.preapproval_set_up ? "Yes" : "No"}
                  size="small"
                  color={h.preapproval_set_up ? "success" : "default"}
                />
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </Box>
  );
};
