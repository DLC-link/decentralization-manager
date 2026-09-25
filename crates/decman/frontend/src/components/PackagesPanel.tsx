import { useState, useRef, useEffect, useCallback, useMemo } from "react";
import {
  Typography,
  Box,
  Skeleton,
  Table,
  TableHead,
  TableBody,
  TableRow,
  TableCell,
  Button,
  CircularProgress,
  TextField,
  Tooltip,
  InputAdornment,
  Autocomplete,
  Chip,
  Alert,
  FormControlLabel,
  Switch,
  IconButton,
  Dialog,
  DialogTitle,
  DialogContent,
  DialogActions,
} from "@mui/material";
import SearchIcon from "@mui/icons-material/Search";
import CloudUploadIcon from "@mui/icons-material/CloudUpload";
import CompareArrowsIcon from "@mui/icons-material/CompareArrows";
import SignalWifiOffIcon from "@mui/icons-material/SignalWifiOff";
import CheckCircleIcon from "@mui/icons-material/CheckCircle";
import ErrorIcon from "@mui/icons-material/Error";
import SyncProblemIcon from "@mui/icons-material/SyncProblem";
import HelpOutlineIcon from "@mui/icons-material/HelpOutlineOutlined";
import SendIcon from "@mui/icons-material/Send";
import { CopyableText } from "./CopyableText";
import { PaginationControls } from "./Pagination";
import { usePagination } from "../usePagination";
import { ADMIN_ACCESS, API_BASE } from "../constants";
import { useSnackbar } from "../contexts";
import { authenticatedFetch } from "../api";
import { finderTableSx, zebraRow } from "../styles";
import {
  PACKAGE_GROUPS,
  compareCell,
  compareUrl,
  filterTerms,
  indexPeer,
  matchesPackageFilter,
  participantLabel,
  partyParticipants,
  rowDiffers,
  summarize,
  type CellStatus,
  type ParticipantOption,
  canDistribute,
  expectedIndex,
  expectedNotHeld,
} from "../packageCompare";
import type {
  DecentralizedParty,
  DistributePackageRequest,
  ExpectedVersionsResponse,
  PackageInfo,
  VettedPackageInfo,
  PeerPackageComparison,
  PeerPackageResult,
  Peer,
} from "../types";

/// Why this node has no package list for a peer, in the operator's terms.
///
/// The comparison reads the synchronizer's topology store and contacts no
/// peer, so "unreachable" no longer describes any of these. A failed topology
/// read is this node's problem, and an empty vetting set is the peer's, so the
/// tooltip has to separate them: a stale synchronizer-id cache here would
/// otherwise read as every peer being down.
export function peerErrorTooltip(peer: PeerPackageResult): string {
  switch (peer.error_kind) {
    case "topology_read_failed":
      return "This node could not read the synchronizer's topology store — check this node's connection to the synchronizer";
    case "no_vetted_packages":
      return "This peer has vetted no packages — it cannot run any Daml until it uploads and vets them";
    case undefined:
    case null:
      return "No package list for this peer";
    default:
      // The Noise variants, kept on the wire. Nothing emits them now.
      return `No package list for this peer (${peer.error_kind})`;
  }
}

interface PackagesPanelProps {
  onUploadDars?: () => void;
  onDistributeDars?: () => void;
  /// Bumped by the parent after a DAR upload/distribute completes, to trigger
  /// a fresh fetch of the vetted-packages list without a manual refresh.
  refreshNonce?: number;
  selfParticipantId?: string;
  /// Set when the page was opened from a party's Check DARs action. The
  /// participant selection starts as that party's hosts, less this node.
  party?: DecentralizedParty | null;
  onClearParty?: () => void;
}

// Comparison-table columns. Every one is stated, because a fixed-layout table
// hands an unstated column a share of the space rather than the remainder, which
// had left the package name in ~110px. Peers share one width so they read as a
// grid however long their names are, and adding peers pushes the table wider —
// into the scroller — instead of squeezing the name that identifies the row.
const PEER_COL_WIDTH = 150;
const VERSION_COL_WIDTH = 110;
const PACKAGE_MIN_WIDTH = 260;

export const PackagesPanel = ({
  onUploadDars,
  onDistributeDars,
  refreshNonce,
  selfParticipantId,
  party,
  onClearParty,
}: PackagesPanelProps) => {
  const [packages, setPackages] = useState<VettedPackageInfo[]>([]);
  const [loadingPackages, setLoadingPackages] = useState(true);

  useEffect(() => {
    setLoadingPackages(true);
    authenticatedFetch(`${API_BASE}/packages/vetted`)
      .then((res) => (res.ok ? res.json() : []))
      .then((data: VettedPackageInfo[]) => setPackages(data))
      .catch(() => {})
      .finally(() => setLoadingPackages(false));
  }, [refreshNonce]);
  const [canScrollUp, setCanScrollUp] = useState(false);
  const [canScrollDown, setCanScrollDown] = useState(false);
  const [comparison, setComparison] = useState<PeerPackageComparison | null>(
    null,
  );
  const [comparing, setComparing] = useState(false);
  const [search, setSearch] = useState("");
  const [groupKeys, setGroupKeys] = useState<string[]>([]);
  const [differencesOnly, setDifferencesOnly] = useState(false);
  const [distributeTarget, setDistributeTarget] = useState<{
    pkg: PackageInfo;
    peer: PeerPackageResult;
  } | null>(null);
  const [distributing, setDistributing] = useState(false);
  const [expected, setExpected] = useState<ExpectedVersionsResponse | null>(null);
  const [expectedError, setExpectedError] = useState<string | null>(null);
  const { showSnackbar } = useSnackbar();
  const [peers, setPeers] = useState<Peer[]>([]);
  const [selected, setSelected] = useState<string[]>([]);
  const scrollRef = useRef<HTMLDivElement>(null);
  // Answers can arrive out of order when the selection changes quickly. Only
  // the latest request may set the table.
  const compareSeq = useRef(0);

  useEffect(() => {
    authenticatedFetch(`${API_BASE}/network-config`)
      .then((res) => (res.ok ? res.json() : null))
      .then((data: { peers?: Peer[] } | null) => setPeers(data?.peers ?? []))
      .catch(() => {});
  }, []);

  const participantOptions = useMemo(() => {
    const byId = new Map<string, ParticipantOption>();
    for (const p of peers) {
      if (p.participant_id === selfParticipantId) continue;
      byId.set(p.participant_id, { id: p.participant_id, name: p.name });
    }
    for (const id of party ? partyParticipants(party, selfParticipantId) : []) {
      if (!byId.has(id)) byId.set(id, { id, name: "" });
    }
    return [...byId.values()];
  }, [peers, party, selfParticipantId]);

  // Read with every comparison, so the expected versions are as fresh as the
  // vetting they sit beside.
  const loadExpected = useCallback(async () => {
    try {
      const res = await authenticatedFetch(`${API_BASE}/packages/expected-versions`);
      if (res.ok) {
        setExpected(await res.json());
        setExpectedError(null);
      } else {
        const data = await res.json().catch(() => ({}));
        setExpectedError(data.error || `HTTP ${res.status}`);
      }
    } catch (e) {
      setExpectedError(e instanceof Error ? e.message : "request failed");
    }
  }, []);

  const runComparison = useCallback(async (participants: string[]) => {
    const seq = ++compareSeq.current;
    setComparing(true);
    void loadExpected();
    try {
      const res = await authenticatedFetch(compareUrl(participants));
      if (res.ok && seq === compareSeq.current) {
        const data: PeerPackageComparison = await res.json();
        setComparison(data);
      }
    } catch (e) {
      console.error("Failed to compare peer packages:", e);
    } finally {
      if (seq === compareSeq.current) setComparing(false);
    }
  }, [loadExpected]);

  // Opening the page for a party selects its hosts and compares them at once:
  // that comparison is what the Check DARs action asked for.
  // Keyed on the host list, not the party object: the parties poll hands over
  // a new object each tick, and that must not reset the operator's selection.
  const partyHosts = party ? partyParticipants(party, selfParticipantId).join(",") : null;
  useEffect(() => {
    if (partyHosts === null) return;
    const hosts = partyHosts ? partyHosts.split(",") : [];
    setSelected(hosts);
    void runComparison(hosts);
  }, [partyHosts, runComparison]);

  const handleSelectionChange = (ids: string[]) => {
    setSelected(ids);
    // A comparison on screen must describe the selection above it.
    if (comparison) void runComparison(ids);
  };

  const sorted = useMemo(
    () =>
      [...packages].sort((a, b) => {
        const nameCompare = (a.package_name || "").localeCompare(
          b.package_name || "",
        );
        if (nameCompare !== 0) return nameCompare;
        return (a.package_version || "").localeCompare(
          b.package_version || "",
        );
      }),
    [packages],
  );

  const terms = useMemo(() => filterTerms(search), [search]);
  const groups = useMemo(
    () => PACKAGE_GROUPS.filter((g) => groupKeys.includes(g.key)),
    [groupKeys],
  );
  const filterActive = terms.length > 0 || groups.length > 0;

  const filteredSorted = useMemo(
    () =>
      sorted.filter((p) =>
        matchesPackageFilter(
          { name: p.package_name || "", package_id: p.package_id || "" },
          groups,
          terms,
        ),
      ),
    [sorted, groups, terms],
  );

  const peerIndexes = useMemo(
    () => (comparison ? comparison.peers.map(indexPeer) : []),
    [comparison],
  );

  // The package set the operator asked about, before the differences-only
  // switch: the summary describes this set, not just the rows left on screen.
  const selectedPackages = useMemo(() => {
    if (!comparison) return null;
    return comparison.local_packages.filter((p) =>
      matchesPackageFilter(p, groups, terms),
    );
  }, [comparison, groups, terms]);

  const expectedByName = useMemo(
    () => expectedIndex(expected?.packages ?? []),
    [expected],
  );

  // Only for the packages the filter selects, like the rows.
  const expectedMissingHere = useMemo(() => {
    if (!comparison) return [];
    return expectedNotHeld(expected?.packages ?? [], comparison.local_packages).filter((e) =>
      matchesPackageFilter({ name: e.package_name, package_id: "" }, groups, terms),
    );
  }, [comparison, expected, groups, terms]);

  const summary = useMemo(
    () => (selectedPackages ? summarize(selectedPackages, peerIndexes) : null),
    [selectedPackages, peerIndexes],
  );

  const filteredComparison = useMemo(() => {
    if (!selectedPackages) return null;
    if (!differencesOnly) return selectedPackages;
    return selectedPackages.filter((p) => rowDiffers(p, peerIndexes));
  }, [selectedPackages, differencesOnly, peerIndexes]);

  const sortedComparison = useMemo(
    () => [...(filteredComparison ?? [])].sort((a, b) => a.name.localeCompare(b.name)),
    [filteredComparison],
  );

  // This panel scrolls its rows in `scrollRef`, not the window, so paging has to
  // reset that container rather than the document.
  const localPaging = usePagination(filteredSorted, scrollRef);
  const comparisonPaging = usePagination(sortedComparison, scrollRef);
  // The peer-comparison view swaps in its own table, so it pages its own rows.
  const paging = comparison ? comparisonPaging : localPaging;

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
      return () => el.removeEventListener("scroll", updateScrollShadows);
    }
  }, [sorted, comparison, updateScrollShadows]);

  const handleComparePeers = () => runComparison(selected);

  const statusColor = (status: CellStatus, rowIndex: number): string => {
    const even = rowIndex % 2 === 0;
    switch (status) {
      case "match":
        return even ? "rgba(76, 175, 80, 0.08)" : "rgba(76, 175, 80, 0.15)";
      case "other_version":
        return even ? "rgba(255, 152, 0, 0.10)" : "rgba(255, 152, 0, 0.18)";
      case "missing":
        return even ? "rgba(244, 67, 54, 0.08)" : "rgba(244, 67, 54, 0.15)";
      case "unknown":
      case "unreachable":
        return even ? "transparent" : "action.hover";
    }
  };

  const configuredPeerIds = useMemo(
    () => new Set(peers.map((p) => p.participant_id)),
    [peers],
  );

  const handleDistribute = async () => {
    if (!distributeTarget) return;
    const { pkg, peer } = distributeTarget;
    setDistributing(true);
    try {
      const body: DistributePackageRequest = {
        package_id: pkg.package_id,
        peer_ids: [peer.participant_id],
      };
      const res = await authenticatedFetch(`${API_BASE}/dars/distribute-package`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      });
      if (res.ok) {
        showSnackbar(
          `Distribution of ${pkg.name} started — follow progress in the feed`,
        );
        setDistributeTarget(null);
      } else {
        const data = await res.json().catch(() => ({}));
        showSnackbar(data.error || "Failed to start the distribution", "error");
      }
    } catch (e) {
      showSnackbar(
        e instanceof Error ? e.message : "Failed to start the distribution",
        "error",
      );
    } finally {
      setDistributing(false);
    }
  };

  const toggleGroup = (key: string) =>
    setGroupKeys((prev) =>
      prev.includes(key) ? prev.filter((k) => k !== key) : [...prev, key],
    );

  return (
    <Box sx={{ display: "flex", flexDirection: "column", flex: 1, minHeight: 0 }}>
      <Box sx={{ display: "flex", justifyContent: "space-between", alignItems: "center", gap: 2, mb: 2, flexShrink: 0, px: "24px", pt: 2 }}>
        <Box sx={{ display: "flex", alignItems: "center", gap: 2, flex: 1, minWidth: 0 }}>
          <TextField
            size="small"
            multiline
            maxRows={4}
            placeholder="Search packages — paste several names"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            slotProps={{
              input: {
                startAdornment: (
                  <InputAdornment position="start">
                    <SearchIcon fontSize="small" />
                  </InputAdornment>
                ),
              },
            }}
            sx={{ width: 320 }}
            data-testid="package-filter"
          />
          {PACKAGE_GROUPS.map((g) => (
            <Chip
              key={g.key}
              label={g.label}
              size="small"
              clickable
              color={groupKeys.includes(g.key) ? "primary" : "default"}
              variant={groupKeys.includes(g.key) ? "filled" : "outlined"}
              onClick={() => toggleGroup(g.key)}
              data-testid={`package-group-${g.key}`}
            />
          ))}
          <Typography variant="body2" color="text.secondary" noWrap>
            {filterActive
              ? `${comparison ? (selectedPackages?.length ?? 0) : filteredSorted.length} of ${comparison ? comparison.local_packages.length : packages.length} packages`
              : `${packages.length} packages vetted on this participant`}
          </Typography>
        </Box>
        <Box sx={{ display: "flex", gap: 1 }}>
          <Button
            variant="outlined"
            size="small"
            startIcon={
              comparing ? (
                <CircularProgress size={16} />
              ) : (
                <CompareArrowsIcon />
              )
            }
            onClick={handleComparePeers}
            disabled={comparing}
          >
            {comparing ? "Checking..." : "Check Peer DARs"}
          </Button>
          {onUploadDars && (
            <Button
              variant="contained"
              size="small"
              color="secondary"
              startIcon={<CloudUploadIcon />}
              onClick={onUploadDars}
            >
              Upload DARs
            </Button>
          )}
          {onDistributeDars && (
            <Button
              variant="contained"
              size="small"
              color="secondary"
              startIcon={<CloudUploadIcon />}
              onClick={onDistributeDars}
            >
              Distribute DARs
            </Button>
          )}
        </Box>
      </Box>

      <Box sx={{ display: "flex", alignItems: "center", gap: 2, mb: 2, flexShrink: 0, px: "24px", flexWrap: "wrap" }}>
        <Autocomplete
          multiple
          size="small"
          options={participantOptions}
          value={participantOptions.filter((o) => selected.includes(o.id))}
          onChange={(_, value) => handleSelectionChange(value.map((o) => o.id))}
          getOptionLabel={participantLabel}
          isOptionEqualToValue={(a, b) => a.id === b.id}
          filterSelectedOptions
          renderOption={(props, option) => {
            const { key, ...rest } = props;
            return (
              <li key={key} {...rest}>
                <Tooltip title={option.id} placement="right" arrow>
                  <span>{participantLabel(option)}</span>
                </Tooltip>
              </li>
            );
          }}
          renderInput={(params) => (
            <TextField
              {...params}
              label="Participants"
              placeholder={selected.length === 0 ? "All peers" : undefined}
            />
          )}
          sx={{ minWidth: 360, flex: 1, maxWidth: 720 }}
          data-testid="participant-filter"
        />
        {party && (
          <Chip
            label={`Party: ${party.party_id.split("::")[0]}`}
            onDelete={onClearParty}
            size="small"
            data-testid="party-scope-chip"
          />
        )}
        {comparison && (
          <FormControlLabel
            control={
              <Switch
                size="small"
                checked={differencesOnly}
                onChange={(e) => setDifferencesOnly(e.target.checked)}
              />
            }
            label="Differences only"
          />
        )}
      </Box>
      {comparison && selected.length === 0 && (
        <Alert severity="info" sx={{ mx: "24px", mb: 2, flexShrink: 0 }}>
          Comparing with every configured peer. Select participants to narrow the comparison.
        </Alert>
      )}
      {comparison && expectedMissingHere.length > 0 && (
        <Alert severity="warning" sx={{ mx: "24px", mb: 2, flexShrink: 0 }} data-testid="expected-not-held">
          {`This node does not hold the expected version of ${expectedMissingHere
            .map((e) => `${e.package_name} ${e.version}`)
            .join(", ")}.`}
        </Alert>
      )}
      {comparison && expectedError && (
        <Alert severity="info" sx={{ mx: "24px", mb: 2, flexShrink: 0 }}>
          {`Expected versions are unavailable: ${expectedError}. Observed versions are still compared.`}
        </Alert>
      )}
      {summary && (
        <Box sx={{ mx: "24px", mb: 2, flexShrink: 0 }} data-testid="comparison-summary">
          {summary.differing === 0 && summary.missing === 0 ? (
            <Alert severity="success">
              No version differences found for the selected participants and package set.
              {summary.unavailable > 0 &&
                ` ${summary.unavailable} participant(s) had no package list and were not compared.`}
            </Alert>
          ) : (
            <Alert severity="warning">
              {`${summary.differing} other version(s) and ${summary.missing} missing package(s) across ${summary.packages} package(s).`}
              {summary.unavailable > 0 &&
                ` ${summary.unavailable} participant(s) had no package list and were not compared.`}
            </Alert>
          )}
        </Box>
      )}

        <Box sx={{ position: "relative", flex: 1, minHeight: 0, display: "flex", flexDirection: "column" }}>
          <Box
            sx={{
              position: "absolute",
              top: 0,
              left: 0,
              right: 0,
              height: 16,
              background:
                "linear-gradient(to bottom, rgba(0,0,0,0.08), transparent)",
              pointerEvents: "none",
              opacity: canScrollUp ? 1 : 0,
              transition: "opacity 0.2s",
              zIndex: 1,
            }}
          />
          <Box
            ref={scrollRef}
            sx={{
              flex: 1,
              minHeight: 0,
              overflowY: "auto",
              overflowX: "auto",
            }}
          >
            {loadingPackages ? (
              <Table size="small" sx={{ minWidth: 650, ...finderTableSx }}>
                <TableHead>
                  <TableRow>
                    <TableCell sx={{ py: 1 }}><Skeleton width="60%" /></TableCell>
                    <TableCell sx={{ py: 1 }}><Skeleton width={50} /></TableCell>
                    <TableCell sx={{ py: 1 }}><Skeleton width="70%" /></TableCell>
                  </TableRow>
                </TableHead>
                <TableBody>
                  {Array.from({ length: 20 }).map((_, i) => (
                    <TableRow key={i} sx={zebraRow(i)}>
                      <TableCell sx={{ py: 1 }}><Skeleton width={`${50 + (i % 3) * 15}%`} /></TableCell>
                      <TableCell sx={{ py: 1 }}><Skeleton width={40} /></TableCell>
                      <TableCell sx={{ py: 1 }}><Skeleton width={`${55 + (i % 4) * 10}%`} /></TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            ) : comparison ? (
              /* Comparison table */
              <Table
                size="small"
                sx={{
                  // Fixed rather than auto: auto treats a column width as a
                  // suggestion and takes the shortfall out of whichever column
                  // it likes, which left the last peer squeezed against its
                  // neighbours. Fixed honours them, so every peer is one width.
                  tableLayout: "fixed",
                  // The theme pads a table's leading/trailing cell out to
                  // `--content-pad`. This table isn't full-bleed — it's a fixed
                  // grid inside a scroller — and that padding ate the last peer
                  // column, leaving its tick sitting against the column's left
                  // edge while the column itself painted full width. The Finder
                  // rule replaces both with a fixed gutter, so the package name
                  // stays at the left of the pane at any window width.
                  ...finderTableSx,
                  // Grows with the peer count, so the columns keep their width
                  // and the table overflows into the scroller instead of every
                  // column shrinking as peers are added.
                  minWidth:
                    PACKAGE_MIN_WIDTH +
                    2 * VERSION_COL_WIDTH +
                    peerIndexes.length * PEER_COL_WIDTH,
                }}
              >
                {/* Columns are declared here rather than inferred from the
                  * header cells: sizing a fixed-layout table off its cells left
                  * the last peer's column painting one width while laying its
                  * content out at another. The package column is left open so it
                  * takes whatever the stated columns leave. */}
                <colgroup>
                  <col />
                  <col style={{ width: VERSION_COL_WIDTH }} />
                  <col style={{ width: VERSION_COL_WIDTH }} />
                  {peerIndexes.map(({ peer }) => (
                    <col key={peer.participant_id} style={{ width: PEER_COL_WIDTH }} />
                  ))}
                </colgroup>
                <TableHead>
                  <TableRow>
                    <TableCell sx={{ py: 1, fontWeight: "bold" }}>
                      Package
                    </TableCell>
                    <TableCell sx={{ py: 1, fontWeight: "bold" }}>
                      Version
                    </TableCell>
                    <TableCell sx={{ py: 1, fontWeight: "bold" }}>
                      <Tooltip
                        title={
                          expected
                            ? `From the ${expected.source}, read ${new Date(expected.fetched_at * 1000).toLocaleString()}. Only Splice packages have an expected version.`
                            : "Only Splice packages have an expected version"
                        }
                        arrow
                      >
                        <span>Expected</span>
                      </Tooltip>
                    </TableCell>
                    {peerIndexes.map(({ peer }) => (
                      <TableCell
                        key={peer.participant_id}
                        sx={{
                          py: 1,
                          fontWeight: "bold",
                          textAlign: "center",
                          opacity: peer.reachable ? 1 : 0.5,
                          whiteSpace: "nowrap",
                        }}
                      >
                        <Box
                          sx={{
                            display: "flex",
                            alignItems: "center",
                            justifyContent: "center",
                            gap: 0.5,
                          }}
                        >
                          <Tooltip title={peer.participant_id} arrow>
                            <Box
                              component="span"
                              sx={{
                                minWidth: 0,
                                overflow: "hidden",
                                textOverflow: "ellipsis",
                                whiteSpace: "nowrap",
                              }}
                            >
                              {peer.name || peer.participant_id}
                            </Box>
                          </Tooltip>
                          {!peer.reachable && (
                            <Tooltip title={peerErrorTooltip(peer)} arrow>
                              <SignalWifiOffIcon
                                sx={{
                                  fontSize: 14,
                                  color: "text.disabled",
                                  flexShrink: 0,
                                }}
                              />
                            </Tooltip>
                          )}
                        </Box>
                      </TableCell>
                    ))}
                  </TableRow>
                </TableHead>
                <TableBody>
                  {comparisonPaging.pageItems
                    .map((pkg, idx) => (
                      <TableRow key={pkg.package_id} sx={zebraRow(idx)}>
                        <TableCell
                          sx={{
                            py: 1,
                            overflow: "hidden",
                            textOverflow: "ellipsis",
                            whiteSpace: "nowrap",
                          }}
                        >
                          {pkg.name || "-"}
                        </TableCell>
                        <TableCell sx={{ py: 1 }}>
                          {pkg.version || "-"}
                        </TableCell>
                        <TableCell
                          sx={{ py: 1 }}
                          data-testid="expected-version"
                          data-pkg={pkg.name}
                        >
                          {(() => {
                            const want = expectedByName.get(pkg.name);
                            if (!want) {
                              return (
                                <Tooltip
                                  title="No trusted source gives an expected version for this package"
                                  arrow
                                >
                                  <Typography variant="caption" color="text.disabled">
                                    —
                                  </Typography>
                                </Tooltip>
                              );
                            }
                            return (
                              <Typography
                                variant="body2"
                                component="span"
                                sx={{
                                  color: want === pkg.version ? "success.main" : "text.secondary",
                                  fontWeight: want === pkg.version ? 600 : 400,
                                }}
                              >
                                {want}
                              </Typography>
                            );
                          })()}
                        </TableCell>
                        {peerIndexes.map((index) => {
                          const { peer, unnamed } = index;
                          const { status, versions } = compareCell(index, pkg);
                          const missingHint =
                            unnamed > 0
                              ? ` It vets ${unnamed} package(s) this node does not hold, so it may have a version this node lacks.`
                              : "";
                          return (
                            <TableCell
                              key={peer.participant_id}
                              data-testid="peer-dar-status"
                              data-pkg={pkg.name}
                              data-status={status}
                              sx={{
                                py: 1,
                                textAlign: "center",
                                bgcolor: statusColor(status, idx),
                              }}
                            >
                              {status === "match" && (
                                <Tooltip title="Matches local package" arrow>
                                  <CheckCircleIcon
                                    sx={{ fontSize: 16, color: "success.main" }}
                                  />
                                </Tooltip>
                              )}
                              {status === "other_version" && (
                                <Tooltip
                                  title={`Vets ${versions.join(", ")}, not ${pkg.version}`}
                                  arrow
                                >
                                  <Box
                                    component="span"
                                    sx={{
                                      display: "inline-flex",
                                      alignItems: "center",
                                      gap: 0.5,
                                      color: "warning.main",
                                    }}
                                  >
                                    <SyncProblemIcon sx={{ fontSize: 16 }} />
                                    <Typography variant="caption" noWrap>
                                      {versions.join(", ")}
                                    </Typography>
                                  </Box>
                                </Tooltip>
                              )}
                              {status === "missing" && (
                                <Tooltip
                                  title={`No version of this package is vetted.${missingHint}`}
                                  arrow
                                >
                                  <ErrorIcon
                                    sx={{ fontSize: 16, color: "error.main" }}
                                  />
                                </Tooltip>
                              )}
                              {canDistribute(status) && ADMIN_ACCESS && (
                                <Tooltip
                                  title={
                                    configuredPeerIds.has(peer.participant_id)
                                      ? `Distribute ${pkg.name} ${pkg.version} to ${participantLabel({ id: peer.participant_id, name: peer.name })}`
                                      : "Not a configured peer: DARs can only be distributed to configured peers"
                                  }
                                  arrow
                                >
                                  <span>
                                    <IconButton
                                      size="small"
                                      aria-label={`Distribute ${pkg.name} to ${participantLabel({ id: peer.participant_id, name: peer.name })}`}
                                      disabled={!configuredPeerIds.has(peer.participant_id)}
                                      onClick={() => setDistributeTarget({ pkg, peer })}
                                      sx={{ ml: 0.5, p: 0.25 }}
                                      data-testid="distribute-dar"
                                    >
                                      <SendIcon sx={{ fontSize: 14 }} />
                                    </IconButton>
                                  </span>
                                </Tooltip>
                              )}
                              {status === "unknown" && (
                                <Tooltip
                                  title="This node has no name for this package, so only its id can be compared, and the id does not match"
                                  arrow
                                >
                                  <HelpOutlineIcon
                                    sx={{ fontSize: 16, color: "text.disabled" }}
                                  />
                                </Tooltip>
                              )}
                              {status === "unreachable" && (
                                <Tooltip title={peerErrorTooltip(peer)} arrow>
                                  <Typography
                                    variant="caption"
                                    color="text.disabled"
                                  >
                                    unavailable
                                  </Typography>
                                </Tooltip>
                              )}
                            </TableCell>
                          );
                        })}
                      </TableRow>
                    ))}
                </TableBody>
              </Table>
            ) : (
              /* Default local-only table */
              <Table
                size="small"
                sx={{ minWidth: 650, tableLayout: "fixed", ...finderTableSx }}
              >
                <TableHead>
                  <TableRow>
                    <TableCell sx={{ py: 1, width: "48%" }}>Package Name</TableCell>
                    <TableCell sx={{ py: 1, width: "16%" }}>Version</TableCell>
                    <TableCell sx={{ py: 1, width: "36%" }}>Package ID</TableCell>
                  </TableRow>
                </TableHead>
                <TableBody>
                  {localPaging.pageItems.map((p, idx) => (
                    <TableRow key={p.package_id} sx={zebraRow(idx)}>
                      <TableCell sx={{ py: 1 }}>
                        {p.package_name || "-"}
                      </TableCell>
                      <TableCell sx={{ py: 1 }}>
                        {p.package_version || "-"}
                      </TableCell>
                      <TableCell sx={{ py: 1 }}>
                        <CopyableText
                          text={p.package_id}
                          truncate={{ start: 16, end: 16 }}
                          variant="body2"
                        />
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            )}
          </Box>
          <Box
            sx={{
              position: "absolute",
              bottom: 0,
              left: 0,
              right: 0,
              height: 16,
              background:
                "linear-gradient(to top, rgba(0,0,0,0.08), transparent)",
              pointerEvents: "none",
              opacity: canScrollDown ? 1 : 0,
              transition: "opacity 0.2s",
              zIndex: 1,
            }}
          />
        </Box>
        <PaginationControls
          page={paging.page}
          pageCount={paging.pageCount}
          total={paging.total}
          onChange={paging.setPage}
          sx={{ px: 3 }}
        />
      <Dialog
        open={distributeTarget !== null}
        onClose={() => !distributing && setDistributeTarget(null)}
        maxWidth="xs"
        fullWidth
      >
        <DialogTitle>Distribute DAR</DialogTitle>
        <DialogContent>
          {distributeTarget && (
            <Typography variant="body2">
              {`Send the DAR that holds ${distributeTarget.pkg.name} ${distributeTarget.pkg.version} to ${participantLabel({ id: distributeTarget.peer.participant_id, name: distributeTarget.peer.name })}? The operator of that node accepts or rejects it in their feed.`}
            </Typography>
          )}
        </DialogContent>
        <DialogActions>
          <Button onClick={() => setDistributeTarget(null)} disabled={distributing}>
            Cancel
          </Button>
          <Button
            variant="contained"
            onClick={handleDistribute}
            disabled={distributing}
            startIcon={distributing ? <CircularProgress size={16} /> : <SendIcon />}
          >
            Distribute
          </Button>
        </DialogActions>
      </Dialog>
    </Box>
  );
};
