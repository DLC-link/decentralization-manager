import { useState, useCallback, useEffect, useRef, useMemo, Fragment } from "react";
import {
  Box,
  Typography,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableRow,
  Alert,
  Button,
  Chip,
  IconButton,
  Collapse,
  Tooltip,
  useTheme,
} from "@mui/material";
import RefreshIcon from "@mui/icons-material/Refresh";
import ExpandMoreIcon from "@mui/icons-material/ExpandMore";
import ExpandLessIcon from "@mui/icons-material/ExpandLess";
import ContentCopyIcon from "@mui/icons-material/ContentCopy";
import { JSONTree } from "react-json-tree";
import { API_BASE, PAGE_SIZE } from "../constants";
import { authenticatedFetch } from "../api";
import { zebraRow } from "../styles";
import { CopyableText } from "./CopyableText";
import { CursorPagination } from "./Pagination";
import type { ChainAuditEntry, ChainAuditResponse } from "../types";

interface GovernanceAuditTrailProps {
  partyId: string;
  /// Bumped by the parent after a sibling governance action mutates state, OR
  /// when the operator clicks the Refresh icon in the section header — both
  /// trigger a fresh fetch.
  refreshNonce?: number;
  /// Reports the loaded entry count to the parent so it can render a badge in
  /// the section header (matches the Contracts pattern). `hasMore` says whether
  /// older entries exist beyond this page — a page can legally hold more than
  /// `CHAIN_LIMIT` rows, so the count alone can't tell the parent that.
  onCountChange?: (count: number, hasMore: boolean) => void;
  /// Reports fetch in-flight state up so the parent can disable its Refresh
  /// icon while a request is pending.
  onLoadingChange?: (loading: boolean) => void;
}

export const CHAIN_LIMIT = PAGE_SIZE;

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

const useJsonTreeTheme = () => {
  const theme = useTheme();
  const dark = theme.palette.mode === "dark";
  return useMemo(
    () => ({
      scheme: "custom",
      base00: "transparent",
      base01: dark ? "#424242" : "#e0e0e0",
      base02: dark ? "#424242" : "#e0e0e0",
      base03: dark ? "#bdbdbd" : "#9e9e9e",
      base04: dark ? "#bdbdbd" : "#9e9e9e",
      base05: dark ? "#e0e0e0" : "#212121",
      base06: dark ? "#e0e0e0" : "#212121",
      base07: dark ? "#e0e0e0" : "#212121",
      base08: dark ? "#ef5350" : "#d32f2f",
      base09: dark ? "#ff9100" : "#e65100",
      base0A: dark ? "#ffee58" : "#f9a825",
      base0B: dark ? "#66bb6a" : "#2e7d32",
      base0C: dark ? "#4dd0e1" : "#00838f",
      base0D: dark ? "#42a5f5" : "#1565c0",
      base0E: dark ? "#ce93d8" : "#9c27b0",
      base0F: dark ? "#ff9100" : "#e65100",
    }),
    [dark],
  );
};

const CopyButton = ({
  data,
  label,
  size = "small",
}: {
  data: unknown;
  label: string;
  size?: "small" | "medium";
}) => {
  const [copied, setCopied] = useState(false);
  const text =
    typeof data === "string" ? data : JSON.stringify(data, null, 2);
  return (
    <Tooltip title={copied ? "Copied!" : label}>
      <IconButton
        size={size}
        onClick={() => {
          navigator.clipboard.writeText(text);
          setCopied(true);
          setTimeout(() => setCopied(false), 1500);
        }}
      >
        <ContentCopyIcon fontSize="small" />
      </IconButton>
    </Tooltip>
  );
};

export const GovernanceAuditTrail = ({
  partyId,
  refreshNonce,
  onCountChange,
  onLoadingChange,
}: GovernanceAuditTrailProps) => {
  const jsonTreeTheme = useJsonTreeTheme();
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [entries, setEntries] = useState<ChainAuditEntry[]>([]);
  const sortedEntries = useMemo(
    () => [...entries].sort((a, b) => b.timestamp - a.timestamp),
    [entries],
  );
  const [expandedRow, setExpandedRow] = useState<string | null>(null);
  const [cacheLoaded, setCacheLoaded] = useState(false);
  // One cursor per visited page. Index 0 is `undefined` (newest page); each
  // Next pushes the `next_before_offset` the server handed back, so Previous
  // is a pop rather than a re-query from the top.
  const [cursors, setCursors] = useState<(number | undefined)[]>([undefined]);
  const [page, setPage] = useState(0);
  const [nextCursor, setNextCursor] = useState<number | null>(null);
  const [canScrollUp, setCanScrollUp] = useState(false);
  const [canScrollDown, setCanScrollDown] = useState(false);
  const scrollRef = useRef<HTMLDivElement>(null);

  const updateScrollShadows = useCallback(() => {
    const el = scrollRef.current;
    if (el) {
      setCanScrollUp(el.scrollTop > 0);
      setCanScrollDown(el.scrollTop < el.scrollHeight - el.clientHeight - 1);
    }
  }, []);

  useEffect(() => {
    const el = scrollRef.current;
    if (el) {
      updateScrollShadows();
      el.addEventListener("scroll", updateScrollShadows);
      const observer = new ResizeObserver(updateScrollShadows);
      observer.observe(el);
      return () => {
        el.removeEventListener("scroll", updateScrollShadows);
        observer.disconnect();
      };
    }
  }, [entries, updateScrollShadows]);

  const fetchAudit = useCallback(
    async (refresh: boolean, before?: number): Promise<boolean> => {
      setLoading(true);
      setError(null);
      // A refresh pulls in events newer than anything we've paged past, which
      // invalidates every cursor below — start the stack over at the top.
      if (refresh && before === undefined) {
        setCursors([undefined]);
        setPage(0);
      }
      try {
        const params = new URLSearchParams({
          party_id: partyId,
          limit: String(CHAIN_LIMIT),
        });
        if (refresh) params.set("refresh", "true");
        if (before !== undefined) params.set("before_offset", String(before));

        const res = await authenticatedFetch(
          `${API_BASE}/governance/chain-audit?${params}`,
        );
        if (res.ok) {
          const response: ChainAuditResponse = await res.json();
          setEntries(response.entries);
          setNextCursor(response.next_before_offset ?? null);
          return true;
        }
        const errData = await res.json().catch(() => ({}));
        setError(errData.error || "Failed to fetch audit trail");
        return false;
      } catch (e) {
        setError(
          e instanceof Error ? e.message : "Failed to fetch audit trail",
        );
        return false;
      } finally {
        setLoading(false);
      }
    },
    [partyId],
  );

  // Load from cache on mount
  useEffect(() => {
    if (!cacheLoaded) {
      setCacheLoaded(true);
      fetchAudit(false);
    }
  }, [cacheLoaded, fetchAudit]);

  // Re-fetch (force fresh) whenever the parent bumps the nonce after a
  // sibling governance mutation.
  useEffect(() => {
    if (refreshNonce === undefined || refreshNonce === 0) return;
    fetchAudit(true);
  }, [refreshNonce, fetchAudit]);

  // A new page is read from its top, same as the client-paged views — see
  // `usePagination`, which scrolls for the same reason.
  const scrollToFirstRow = useCallback(() => {
    scrollRef.current?.scrollTo({ top: 0 });
  }, []);

  // Both advance the page only once the fetch has landed. Moving first would
  // leave the indicator and the cursor stack describing a page whose rows
  // never arrived, and the next Prev would then walk from the wrong cursor.
  const goToNextPage = useCallback(async () => {
    if (nextCursor === null) return;
    const cursor = nextCursor;
    if (await fetchAudit(false, cursor)) {
      setCursors((prev) => [...prev.slice(0, page + 1), cursor]);
      setPage(page + 1);
      scrollToFirstRow();
    }
  }, [nextCursor, page, fetchAudit, scrollToFirstRow]);

  const goToPrevPage = useCallback(async () => {
    if (page === 0) return;
    if (await fetchAudit(false, cursors[page - 1])) {
      setPage(page - 1);
      scrollToFirstRow();
    }
  }, [page, cursors, fetchAudit, scrollToFirstRow]);

  useEffect(() => {
    onCountChange?.(entries.length, nextCursor !== null);
  }, [entries.length, nextCursor, onCountChange]);

  useEffect(() => {
    onLoadingChange?.(loading);
  }, [loading, onLoadingChange]);

  if (error) {
    return (
      <Box sx={{ mt: 2, mb: 2 }}>
        <Alert
          severity="error"
          sx={{ mb: 2 }}
          onClose={() => setError(null)}
        >
          {error}
        </Alert>
        <Button
          startIcon={<RefreshIcon />}
          onClick={() => fetchAudit(true)}
          size="small"
          variant="outlined"
        >
          Retry
        </Button>
      </Box>
    );
  }

  return (
    <Box>
      {entries.length === 0 ? (
        <Typography variant="body2" color="text.secondary" sx={{ py: 2 }}>
          No on-chain governance events found for this party.
        </Typography>
      ) : (
        <Box sx={{ position: "relative" }}>
          <Box
            sx={{
              position: "absolute",
              top: 0,
              left: 0,
              right: 0,
              height: 16,
              background: "linear-gradient(to bottom, rgba(0,0,0,0.08), transparent)",
              pointerEvents: "none",
              opacity: canScrollUp ? 1 : 0,
              transition: "opacity 0.2s",
              zIndex: 1,
            }}
          />
          <Box
            ref={scrollRef}
            sx={{
              // Viewport-relative cap so the list grows with the window
              // instead of stopping at a hardcoded 400px. Offset accounts for
              // sticky chrome above the table on a typical Parties layout
              // (header chips + owner-key row + collapsed sections + this
              // section's header).
              maxHeight: "calc(100vh - 280px)",
              overflowY: "auto",
              overflowX: "auto",
            }}
          >
          <Table size="small">
            <TableHead>
              <TableRow>
                <TableCell sx={{ py: 1, width: 32 }} />
                <TableCell sx={{ py: 1, whiteSpace: "nowrap" }}>Time</TableCell>
                <TableCell sx={{ py: 1, whiteSpace: "nowrap" }}>Event</TableCell>
                <TableCell sx={{ py: 1, whiteSpace: "nowrap" }}>Action</TableCell>
                <TableCell sx={{ py: 1, whiteSpace: "nowrap" }}>Type</TableCell>
                <TableCell sx={{ py: 1, whiteSpace: "nowrap" }}>
                  Contract
                </TableCell>
              </TableRow>
            </TableHead>
            <TableBody>
              {sortedEntries.map((entry, idx) => {
                const rowKey = `${entry.offset}-${entry.contract_id}`;
                const isExpanded = expandedRow === rowKey;
                return (
                  <Fragment key={rowKey}>
                    <TableRow sx={zebraRow(idx)}>
                      <TableCell sx={{ py: 0.5 }}>
                        <Tooltip title={isExpanded ? "Hide details" : "Show details"}>
                          <IconButton
                            size="small"
                            onClick={() =>
                              setExpandedRow(isExpanded ? null : rowKey)
                            }
                          >
                            {isExpanded ? (
                              <ExpandLessIcon fontSize="small" />
                            ) : (
                              <ExpandMoreIcon fontSize="small" />
                            )}
                          </IconButton>
                        </Tooltip>
                      </TableCell>
                      <TableCell
                        sx={{ py: 1, fontSize: "0.8rem", whiteSpace: "nowrap" }}
                      >
                        {entry.timestamp > 0
                          ? formatTimestamp(entry.timestamp)
                          : "—"}
                      </TableCell>
                      <TableCell sx={{ py: 1, whiteSpace: "nowrap" }}>
                        <Chip
                          label={entry.event_type}
                          size="small"
                          color={eventTypeColor(entry.event_type)}
                        />
                      </TableCell>
                      <TableCell sx={{ py: 1, maxWidth: 320 }}>
                        <Tooltip title={entry.action_summary}>
                          <Typography
                            variant="body2"
                            sx={{
                              fontFamily: "monospace",
                              fontSize: "0.8rem",
                              whiteSpace: "nowrap",
                              overflow: "hidden",
                              textOverflow: "ellipsis",
                            }}
                          >
                            {entry.action_summary}
                          </Typography>
                        </Tooltip>
                      </TableCell>
                      <TableCell
                        sx={{ py: 1, fontSize: "0.8rem", whiteSpace: "nowrap" }}
                      >
                        {entry.governance_type}
                      </TableCell>
                      <TableCell sx={{ py: 1, whiteSpace: "nowrap" }}>
                        <CopyableText
                          text={entry.contract_id}
                          truncate={{ start: 8, end: 8 }}
                          variant="caption"
                        />
                      </TableCell>
                    </TableRow>
                    <TableRow>
                      <TableCell
                        colSpan={6}
                        sx={{ py: 0, border: 0, maxWidth: 0, ...zebraRow(idx) }}
                      >
                        <Collapse in={isExpanded} timeout="auto" unmountOnExit>
                          <Box sx={{ p: 2, overflow: "hidden" }}>
                            <Typography
                              variant="caption"
                              color="text.secondary"
                              component="div"
                            >
                              Template: {entry.template_id}
                            </Typography>
                            <Typography
                              variant="caption"
                              color="text.secondary"
                              component="div"
                            >
                              Package: {entry.package_id}
                            </Typography>
                            <Typography
                              variant="caption"
                              color="text.secondary"
                              component="div"
                            >
                              Acting parties: {entry.acting_parties.join(", ")}
                            </Typography>
                            <Typography
                              variant="caption"
                              color="text.secondary"
                              component="div"
                            >
                              Update ID: {entry.update_id}
                            </Typography>
                            {entry.choice && (
                              <Typography
                                variant="caption"
                                color="text.secondary"
                                component="div"
                              >
                                Choice: {entry.choice}
                              </Typography>
                            )}
                            {entry.details != null && Object.keys(entry.details).length > 0 && <Box
                              sx={{
                                mt: 1,
                                p: 1.5,
                                bgcolor: "action.hover",
                                borderRadius: 1,
                                overflowX: "auto",
                                overflowY: "auto",
                                maxHeight: 300,
                                fontSize: "0.8rem",
                                position: "relative",
                              }}
                            >
                              <Box sx={{ position: "absolute", top: 4, right: 4, zIndex: 1 }}>
                                <CopyButton data={entry.details} label="Copy JSON" />
                              </Box>
                              <JSONTree
                                data={entry.details}
                                theme={jsonTreeTheme}
                                invertTheme={false}
                                hideRoot
                                shouldExpandNodeInitially={(_keyPath, _data, level) => level < 2}
                                valueRenderer={(raw, value) => (
                                  <span
                                    style={{ cursor: "pointer" }}
                                    title="Click to copy"
                                    onClick={() => {
                                      const text = typeof value === "string" ? value : String(raw);
                                      navigator.clipboard.writeText(text);
                                    }}
                                  >
                                    {String(raw)}
                                  </span>
                                )}
                              />
                            </Box>}
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
          <Box
            sx={{
              position: "absolute",
              bottom: 0,
              left: 0,
              right: 0,
              height: 16,
              background: "linear-gradient(to top, rgba(0,0,0,0.08), transparent)",
              pointerEvents: "none",
              opacity: canScrollDown ? 1 : 0,
              transition: "opacity 0.2s",
              zIndex: 1,
            }}
          />
        </Box>
      )}
      <CursorPagination
        page={page}
        hasNext={nextCursor !== null}
        disabled={loading}
        onPrev={goToPrevPage}
        onNext={goToNextPage}
      />
    </Box>
  );
};
