import { useState, useCallback, Fragment } from "react";
import {
  Box,
  Typography,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableRow,
  CircularProgress,
  Alert,
  Button,
  Chip,
  IconButton,
  Collapse,
  Tooltip,
} from "@mui/material";
import RefreshIcon from "@mui/icons-material/Refresh";
import ExpandMoreIcon from "@mui/icons-material/ExpandMore";
import ExpandLessIcon from "@mui/icons-material/ExpandLess";
import { API_BASE } from "../constants";
import { zebraRow } from "../styles";
import type { AuditLogEntry, AuditLogResponse } from "../types";

interface GovernanceAuditTrailProps {
  partyId: string;
}

const PAGE_SIZE = 50;

const formatTimestamp = (epochSeconds: number): string =>
  new Date(epochSeconds * 1000).toLocaleString();

const eventTypeColor = (
  eventType: string,
): "default" | "primary" | "success" | "warning" | "error" | "info" => {
  switch (eventType) {
    case "propose":
      return "primary";
    case "confirm":
      return "info";
    case "execute":
      return "success";
    case "expire":
      return "warning";
    case "cancel":
      return "error";
    default:
      return "default";
  }
};

export const GovernanceAuditTrail = ({ partyId }: GovernanceAuditTrailProps) => {
  const [loading, setLoading] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [entries, setEntries] = useState<AuditLogEntry[]>([]);
  const [page, setPage] = useState(0);
  const [hasMore, setHasMore] = useState(false);
  const [expandedRow, setExpandedRow] = useState<number | null>(null);

  const fetchAuditTrail = useCallback(
    async (targetPage: number) => {
      setLoading(true);
      try {
        const offset = targetPage * PAGE_SIZE;
        const res = await fetch(
          `${API_BASE}/governance/audit?party_id=${encodeURIComponent(
            partyId,
          )}&limit=${PAGE_SIZE}&offset=${offset}`,
        );
        if (res.ok) {
          const response: AuditLogResponse = await res.json();
          setEntries(response.entries);
          setHasMore(response.total_returned === PAGE_SIZE);
          setPage(targetPage);
          setLoaded(true);
          setError(null);
        } else {
          const errData = await res.json().catch(() => ({}));
          setError(errData.error || "Failed to fetch audit trail");
        }
      } catch (e) {
        setError(
          e instanceof Error ? e.message : "Failed to fetch audit trail",
        );
      } finally {
        setLoading(false);
      }
    },
    [partyId],
  );

  if (error) {
    return (
      <Box sx={{ mt: 2 }}>
        <Alert severity="error" sx={{ mb: 2 }}>
          {error}
        </Alert>
        <Button
          startIcon={<RefreshIcon />}
          onClick={() => fetchAuditTrail(page)}
          size="small"
          variant="outlined"
        >
          Retry
        </Button>
      </Box>
    );
  }

  if (!loaded) {
    return (
      <Box
        sx={{
          mt: 2,
          display: "flex",
          flexDirection: "column",
          alignItems: "center",
          py: 4,
          gap: 2,
        }}
      >
        <Typography variant="body2" color="text.secondary">
          Audit trail is not loaded.
        </Typography>
        <Button
          startIcon={loading ? <CircularProgress size={16} /> : <RefreshIcon />}
          onClick={() => fetchAuditTrail(0)}
          disabled={loading}
          variant="contained"
          size="small"
        >
          Load Audit Trail
        </Button>
      </Box>
    );
  }

  return (
    <Box sx={{ mt: 1 }}>
      <Box
        sx={{
          display: "flex",
          justifyContent: "space-between",
          alignItems: "center",
          mb: 1.5,
        }}
      >
        <Typography variant="subtitle2">
          Audit Trail
          {entries.length > 0 && (
            <Chip
              label={`${entries.length}${hasMore ? "+" : ""}`}
              size="small"
              sx={{ ml: 1 }}
            />
          )}
        </Typography>
        <Button
          startIcon={<RefreshIcon />}
          onClick={() => fetchAuditTrail(page)}
          disabled={loading}
          size="small"
        >
          Refresh
        </Button>
      </Box>

      {entries.length === 0 ? (
        <Typography variant="body2" color="text.secondary" sx={{ py: 2 }}>
          No governance actions recorded yet.
        </Typography>
      ) : (
        <Box sx={{ overflowX: "auto" }}>
          <Table size="small">
            <TableHead>
              <TableRow>
                <TableCell sx={{ py: 1, width: 32 }} />
                <TableCell sx={{ py: 1 }}>Time</TableCell>
                <TableCell sx={{ py: 1 }}>Event</TableCell>
                <TableCell sx={{ py: 1 }}>Action</TableCell>
                <TableCell sx={{ py: 1 }}>Type</TableCell>
                <TableCell sx={{ py: 1 }}>Status</TableCell>
              </TableRow>
            </TableHead>
            <TableBody>
              {entries.map((entry, idx) => {
                const isExpanded = expandedRow === entry.id;
                return (
                  <Fragment key={entry.id}>
                    <TableRow sx={zebraRow(idx)}>
                      <TableCell sx={{ py: 0.5 }}>
                        <IconButton
                          size="small"
                          onClick={() =>
                            setExpandedRow(isExpanded ? null : entry.id)
                          }
                        >
                          {isExpanded ? (
                            <ExpandLessIcon fontSize="small" />
                          ) : (
                            <ExpandMoreIcon fontSize="small" />
                          )}
                        </IconButton>
                      </TableCell>
                      <TableCell sx={{ py: 1, fontSize: "0.8rem" }}>
                        {formatTimestamp(entry.created_at)}
                      </TableCell>
                      <TableCell sx={{ py: 1 }}>
                        <Chip
                          label={entry.event_type}
                          size="small"
                          color={eventTypeColor(entry.event_type)}
                        />
                      </TableCell>
                      <TableCell sx={{ py: 1 }}>
                        <Typography
                          variant="body2"
                          sx={{ fontFamily: "monospace", fontSize: "0.8rem" }}
                        >
                          {entry.action_summary}
                        </Typography>
                      </TableCell>
                      <TableCell sx={{ py: 1, fontSize: "0.8rem" }}>
                        {entry.governance_type}
                      </TableCell>
                      <TableCell sx={{ py: 1 }}>
                        {entry.status === "success" ? (
                          <Chip label="success" size="small" color="success" />
                        ) : (
                          <Tooltip title={entry.error_message ?? ""}>
                            <Chip label="failed" size="small" color="error" />
                          </Tooltip>
                        )}
                      </TableCell>
                    </TableRow>
                    <TableRow>
                      <TableCell
                        colSpan={6}
                        sx={{ py: 0, border: 0, ...zebraRow(idx) }}
                      >
                        <Collapse in={isExpanded} timeout="auto" unmountOnExit>
                          <Box sx={{ p: 2 }}>
                            <Typography
                              variant="caption"
                              color="text.secondary"
                            >
                              Member party: {entry.member_party_id}
                            </Typography>
                            {entry.error_message && (
                              <Alert severity="error" sx={{ mt: 1, mb: 1 }}>
                                {entry.error_message}
                              </Alert>
                            )}
                            <Box
                              component="pre"
                              sx={{
                                mt: 1,
                                p: 1.5,
                                bgcolor: "action.hover",
                                borderRadius: 1,
                                fontSize: "0.75rem",
                                overflow: "auto",
                                maxHeight: 300,
                              }}
                            >
                              {JSON.stringify(entry.details, null, 2)}
                            </Box>
                          </Box>
                        </Collapse>
                      </TableCell>
                    </TableRow>
                  </Fragment>
                );
              })}
            </TableBody>
          </Table>
        </Box>
      )}

      {(page > 0 || hasMore) && (
        <Box
          sx={{
            display: "flex",
            justifyContent: "space-between",
            mt: 2,
          }}
        >
          <Button
            size="small"
            disabled={page === 0 || loading}
            onClick={() => fetchAuditTrail(Math.max(0, page - 1))}
          >
            Previous
          </Button>
          <Typography variant="body2" color="text.secondary">
            Page {page + 1}
          </Typography>
          <Button
            size="small"
            disabled={!hasMore || loading}
            onClick={() => fetchAuditTrail(page + 1)}
          >
            Next
          </Button>
        </Box>
      )}
    </Box>
  );
};
