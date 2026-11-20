import { Typography, Box, Table, TableHead, TableBody, TableRow, TableCell, Tooltip, IconButton } from "@mui/material";
import SignalWifiOffIcon from "@mui/icons-material/SignalWifiOff";
import CheckCircleIcon from "@mui/icons-material/CheckCircle";
import ErrorIcon from "@mui/icons-material/Error";
import SyncProblemIcon from "@mui/icons-material/SyncProblem";
import HelpOutlineIcon from "@mui/icons-material/HelpOutlineOutlined";
import SendIcon from "@mui/icons-material/Send";
import { ADMIN_ACCESS } from "../constants";
import type { Theme } from "@mui/material/styles";
import { finderTableSx, ROW_HOVER_TINT, zebraRow, zebraStripe } from "../styles";
import { compareCell, participantLabel, type CellStatus, type PeerIndex, canDistribute } from "../packageCompare";
import type { ExpectedVersionsResponse, PackageInfo, PeerPackageResult } from "../types";
import { PEER_COL_WIDTH, VERSION_COL_WIDTH, PACKAGE_MIN_WIDTH } from "./packageColumns";
import { peerErrorTooltip } from "./peerErrorTooltip";
import { VersionCell } from "./VersionCell";

// The comparison table's package column stays in view while the peer columns
// scroll sideways. A pinned cell must be opaque, or the columns scrolling under
// it show through, so it paints the row's stripe and hover tint as layers over
// the page background instead of relying on the translucent row colour.
const layer = (color: string) => `linear-gradient(${color}, ${color})`;
const pinnedCellSx = (index: number) => ({
  position: "sticky",
  left: 0,
  zIndex: 1,
  backgroundImage: (theme: Theme) =>
    `${layer(zebraStripe(theme, index))}, ${layer(theme.palette.background.default)}`,
  // A hovered row's other cells show the tint twice over the page, with no
  // stripe, so this one layers it the same way.
  "tr:hover > &": {
    backgroundImage: (theme: Theme) =>
      `${layer(ROW_HOVER_TINT)}, ${layer(ROW_HOVER_TINT)}, ${layer(theme.palette.background.default)}`,
  },
});

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

interface PackageComparisonTableProps {
  peerIndexes: PeerIndex[];
  expected: ExpectedVersionsResponse | null;
  comparisonPaging: { pageItems: PackageInfo[] };
  expectedByName: Map<string, string>;
  configuredPeerIds: Set<string>;
  setDistributeTarget: (target: { pkg: PackageInfo; peer: PeerPackageResult }) => void;
}

const PackageComparisonTable = ({
  peerIndexes,
  expected,
  comparisonPaging,
  expectedByName,
  configuredPeerIds,
  setDistributeTarget,
}: PackageComparisonTableProps) => (
  <Table
    size="small"
    // The header row stays in view while the rows scroll.
    stickyHeader
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
        {/* Pinned left as well as to the top, above the other header cells. */}
        <TableCell sx={{ py: 1, fontWeight: "bold", left: 0, zIndex: 3 }}>
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
              whiteSpace: "nowrap",
            }}
          >
            {/* Dim the contents, not the cell: the header is sticky,
              * and a translucent cell would show the rows under it. */}
            <Box
              sx={{
                display: "flex",
                alignItems: "center",
                justifyContent: "center",
                gap: 0.5,
                opacity: peer.reachable ? 1 : 0.5,
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
                ...pinnedCellSx(idx),
              }}
            >
              {pkg.name || "-"}
            </TableCell>
            <VersionCell version={pkg.version} />
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
                          // Shrinks to the peer column so a long
                          // version ends in an ellipsis; the
                          // tooltip carries the full list.
                          maxWidth: "100%",
                        }}
                      >
                        <SyncProblemIcon sx={{ fontSize: 16, flexShrink: 0 }} />
                        <Typography variant="caption" noWrap sx={{ minWidth: 0 }}>
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
);

export { PackageComparisonTable };
