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
  ToggleButton,
  ToggleButtonGroup,
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
import type {
  AuditLogEntry,
  AuditLogResponse,
  ChainAuditEntry,
  ChainAuditResponse,
} from "../types";

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

/**
 * Which trail the panel shows.
 *
 * `governance` and `all` are the two scopes of the on-ledger read; `local` is
 * this node's own record of what it attempted, which exists even for actions
 * that never reached the ledger.
 */
export type AuditMode = "governance" | "all" | "local";

const MODE_LABELS: { value: AuditMode; label: string; hint: string }[] = [
  {
    value: "governance",
    label: "Governance",
    hint: "Proposals, confirmations and executions from the governance packages",
  },
  {
    value: "all",
    label: "All activity",
    hint: "Every ledger event this party witnesses, its own application templates included",
  },
  {
    value: "local",
    label: "Local log",
    hint: "What this node attempted for the party, successes and failures alike",
  },
];

const EMPTY_MESSAGE: Record<AuditMode, string> = {
  governance:
    "No on-chain governance events found for this party. A party with no governance contracts deployed never produces any — switch to All activity to see its ledger events.",
  all: "No on-chain events found for this party.",
  local: "This node has not run a governance action for this party.",
};

/** One table row, whichever trail it came from. */
interface AuditRow {
  key: string;
  timestamp: number;
  eventType: string;
  action: string;
  /** Fourth column: governance type on-ledger, outcome in the local log. */
  meta: string;
  metaTone?: "success" | "error";
  /** Fifth column: the contract on-ledger, the acting member locally. */
  ident: string;
  details: unknown;
  /** Label/value pairs shown when the row is expanded. */
  facts: [string, string][];
  error?: string;
}

const formatTimestamp = (epochSeconds: number): string =>
  new Date(epochSeconds * 1000).toLocaleString();

const chainRow = (entry: ChainAuditEntry): AuditRow => ({
  key: `${entry.offset}-${entry.contract_id}-${entry.event_type}`,
  timestamp: entry.timestamp,
  eventType: entry.event_type,
  action: entry.action_summary,
  meta: entry.governance_type,
  ident: entry.contract_id,
  details: entry.details,
  facts: [
    ["Template", entry.template_id],
    ["Package", entry.package_id],
    ["Acting parties", entry.acting_parties.join(", ")],
    ["Update ID", entry.update_id],
    ...(entry.choice
      ? ([["Choice", entry.choice]] as [string, string][])
      : []),
  ],
});

const localRow = (entry: AuditLogEntry): AuditRow => ({
  key: `local-${entry.id}`,
  timestamp: entry.timestamp,
  eventType: entry.event_type,
  action: entry.action_summary,
  meta: entry.status,
  metaTone: entry.status === "success" ? "success" : "error",
  ident: entry.member_party_id,
  details: entry.details,
  facts: [["Governance type", entry.governance_type]],
  error: entry.error_message,
});

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
    case "cancel_proposal":
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
  const [mode, setMode] = useState<AuditMode>("governance");
  const modeRef = useRef<AuditMode>("governance");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [rows, setRows] = useState<AuditRow[]>([]);
  const sortedRows = useMemo(
    () => [...rows].sort((a, b) => b.timestamp - a.timestamp),
    [rows],
  );
  const [expandedRow, setExpandedRow] = useState<string | null>(null);
  const [loadedMode, setLoadedMode] = useState<AuditMode | null>(null);
  // One cursor per visited page of an on-ledger trail. Index 0 is `undefined`
  // (newest page); each Next pushes the `next_before_offset` the server handed
  // back, so Previous is a pop rather than a re-query from the top. The local
  // log pages by offset instead and ignores this.
  const [cursors, setCursors] = useState<(number | undefined)[]>([undefined]);
  const [page, setPage] = useState(0);
  const [nextCursor, setNextCursor] = useState<number | null>(null);
  // The local log has no cursor: a full page is what says another one follows.
  const [localHasNext, setLocalHasNext] = useState(false);
  const [canScrollUp, setCanScrollUp] = useState(false);
  const [canScrollDown, setCanScrollDown] = useState(false);
  const scrollRef = useRef<HTMLDivElement>(null);

  const isLocal = mode === "local";
  const hasNext = isLocal ? localHasNext : nextCursor !== null;

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
  }, [rows, updateScrollShadows]);

  const fetchAudit = useCallback(
    async (
      target: AuditMode,
      refresh: boolean,
      // On-ledger cursor, or the page index for the local log.
      before?: number,
      pageIndex = 0,
    ): Promise<boolean> => {
      setLoading(true);
      setError(null);
      // A refresh pulls in events newer than anything we've paged past, which
      // invalidates every cursor below — start the stack over at the top.
      if (refresh && before === undefined && pageIndex === 0) {
        setCursors([undefined]);
        setPage(0);
      }
      try {
        const params = new URLSearchParams({
          party_id: partyId,
          limit: String(CHAIN_LIMIT),
        });

        if (target === "local") {
          params.set("offset", String(pageIndex * CHAIN_LIMIT));
          const res = await authenticatedFetch(
            `${API_BASE}/governance/audit?${params}`,
          );
          if (res.ok) {
            const response: AuditLogResponse = await res.json();
            // The local log pages by offset and reports no total, so a page
            // that comes back exactly full cannot say whether another follows.
            // An empty page past the first is that answer arriving late: it is
            // the end of the log, so stay on the page we are on rather than
            // replacing the table with the empty state.
            if (response.entries.length === 0 && pageIndex > 0) {
              setLocalHasNext(false);
              return false;
            }
            setRows(response.entries.map(localRow));
            setLocalHasNext(response.entries.length >= CHAIN_LIMIT);
            return true;
          }
          const errData = await res.json().catch(() => ({}));
          setError(errData.error || "Failed to fetch audit trail");
          return false;
        }

        params.set("scope", target);
        if (refresh) params.set("refresh", "true");
        if (before !== undefined) params.set("before_offset", String(before));

        const res = await authenticatedFetch(
          `${API_BASE}/governance/chain-audit?${params}`,
        );
        if (res.ok) {
          const response: ChainAuditResponse = await res.json();
          setRows(response.entries.map(chainRow));
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

  // Load on mount, and again whenever the operator switches trail. Switching
  // starts from the newest page: the cursors of the trail left behind mean
  // nothing in the one being entered.
  useEffect(() => {
    if (loadedMode !== mode) {
      setLoadedMode(mode);
      setRows([]);
      setExpandedRow(null);
      setCursors([undefined]);
      setPage(0);
      setNextCursor(null);
      setLocalHasNext(false);
      fetchAudit(mode, false);
    }
  }, [loadedMode, mode, fetchAudit]);

  // Re-fetch (force fresh) whenever the parent bumps the nonce after a
  // sibling governance mutation. The trail is read from `modeRef` — which the
  // toggle writes alongside `setMode` — so the nonce stays the only trigger;
  // depending on `mode` here would re-run this on a switch, on top of the
  // fetch the switch effect above already does.
  useEffect(() => {
    if (refreshNonce === undefined || refreshNonce === 0) return;
    fetchAudit(modeRef.current, true);
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
    if (isLocal) {
      if (!localHasNext) return;
      if (await fetchAudit(mode, false, undefined, page + 1)) {
        setPage(page + 1);
        scrollToFirstRow();
      }
      return;
    }
    if (nextCursor === null) return;
    const cursor = nextCursor;
    if (await fetchAudit(mode, false, cursor)) {
      setCursors((prev) => [...prev.slice(0, page + 1), cursor]);
      setPage(page + 1);
      scrollToFirstRow();
    }
  }, [
    isLocal,
    localHasNext,
    mode,
    nextCursor,
    page,
    fetchAudit,
    scrollToFirstRow,
  ]);

  const goToPrevPage = useCallback(async () => {
    if (page === 0) return;
    const landed = isLocal
      ? await fetchAudit(mode, false, undefined, page - 1)
      : await fetchAudit(mode, false, cursors[page - 1]);
    if (landed) {
      setPage(page - 1);
      scrollToFirstRow();
    }
  }, [isLocal, mode, page, cursors, fetchAudit, scrollToFirstRow]);

  useEffect(() => {
    onCountChange?.(rows.length, hasNext);
  }, [rows.length, hasNext, onCountChange]);

  useEffect(() => {
    onLoadingChange?.(loading);
  }, [loading, onLoadingChange]);

  const modeToggle = (
    <ToggleButtonGroup
      size="small"
      exclusive
      value={mode}
      onChange={(_e, next: AuditMode | null) => {
        if (next) {
          modeRef.current = next;
          setMode(next);
        }
      }}
      sx={{ mb: 1.5 }}
    >
      {MODE_LABELS.map(({ value, label, hint }) => (
        <Tooltip key={value} title={hint}>
          <ToggleButton value={value} sx={{ textTransform: "none", px: 1.5 }}>
            {label}
          </ToggleButton>
        </Tooltip>
      ))}
    </ToggleButtonGroup>
  );

  if (error) {
    return (
      <Box sx={{ mt: 2, mb: 2 }}>
        {modeToggle}
        <Alert
          severity="error"
          sx={{ mb: 2 }}
          onClose={() => setError(null)}
        >
          {error}
        </Alert>
        <Button
          startIcon={<RefreshIcon />}
          onClick={() => fetchAudit(mode, true)}
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
      {modeToggle}
      {rows.length === 0 ? (
        <Typography variant="body2" color="text.secondary" sx={{ py: 2 }}>
          {EMPTY_MESSAGE[mode]}
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
                <TableCell sx={{ py: 1, whiteSpace: "nowrap" }}>
                  {isLocal ? "Status" : "Type"}
                </TableCell>
                <TableCell sx={{ py: 1, whiteSpace: "nowrap" }}>
                  {isLocal ? "Member party" : "Contract"}
                </TableCell>
              </TableRow>
            </TableHead>
            <TableBody>
              {sortedRows.map((row, idx) => {
                const isExpanded = expandedRow === row.key;
                return (
                  <Fragment key={row.key}>
                    <TableRow sx={zebraRow(idx)}>
                      <TableCell sx={{ py: 1 }}>
                        <Tooltip title={isExpanded ? "Hide details" : "Show details"}>
                          <IconButton
                            size="small"
                            onClick={() =>
                              setExpandedRow(isExpanded ? null : row.key)
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
                        {row.timestamp > 0
                          ? formatTimestamp(row.timestamp)
                          : "—"}
                      </TableCell>
                      <TableCell sx={{ py: 1, whiteSpace: "nowrap" }}>
                        <Chip
                          label={row.eventType}
                          size="small"
                          color={eventTypeColor(row.eventType)}
                        />
                      </TableCell>
                      <TableCell sx={{ py: 1, maxWidth: 320 }}>
                        <Tooltip title={row.action}>
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
                            {row.action}
                          </Typography>
                        </Tooltip>
                      </TableCell>
                      <TableCell
                        sx={{ py: 1, fontSize: "0.8rem", whiteSpace: "nowrap" }}
                      >
                        {row.metaTone ? (
                          <Chip
                            label={row.meta}
                            size="small"
                            color={row.metaTone}
                            variant="outlined"
                          />
                        ) : (
                          row.meta
                        )}
                      </TableCell>
                      <TableCell sx={{ py: 1, whiteSpace: "nowrap" }}>
                        <CopyableText
                          text={row.ident}
                          truncate={{ start: 8, end: 8 }}
                          variant="caption"
                        />
                      </TableCell>
                    </TableRow>
                    <TableRow>
                      <TableCell
                        colSpan={6}
                        sx={{ py: 0, height: "auto", border: 0, maxWidth: 0, ...zebraRow(idx) }}
                      >
                        <Collapse in={isExpanded} timeout="auto" unmountOnExit>
                          <Box sx={{ p: 2, overflow: "hidden" }}>
                            {row.facts.map(([label, value]) => (
                              <Typography
                                key={label}
                                variant="caption"
                                color="text.secondary"
                                component="div"
                              >
                                {label}: {value}
                              </Typography>
                            ))}
                            {row.error && (
                              <Typography
                                variant="caption"
                                color="error"
                                component="div"
                              >
                                Error: {row.error}
                              </Typography>
                            )}
                            {row.details != null && Object.keys(row.details).length > 0 && <Box
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
                                <CopyButton data={row.details} label="Copy JSON" />
                              </Box>
                              <JSONTree
                                data={row.details}
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
        hasNext={hasNext}
        disabled={loading}
        onPrev={goToPrevPage}
        onNext={goToNextPage}
      />
    </Box>
  );
};
