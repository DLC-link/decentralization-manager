import {
  Alert,
  AlertTitle,
  Box,
  Chip,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableRow,
  Typography,
} from "@mui/material";
import { CopyableText } from "./CopyableText";
import { cardTableSx, zebraRow } from "../styles";
import type { RewardStatusResponse } from "../types";

interface RewardStatusProps {
  /** The loaded status, or null while it loads or after a failed load. */
  status: RewardStatusResponse | null;
  /** Why the status could not be loaded. */
  error: string | null;
}

/** "0.8000000000" reads as "0.8": the ledger pads decimals to ten places. */
const formatShare = (share: string) =>
  share.includes(".") ? share.replace(/0+$/, "").replace(/\.$/, "") : share;

/**
 * Reward coupon reassignment for one party, as this node sees it. It tells
 * apart a missing rewards DAR, a missing delegation, and a delegation that
 * lists other assigners.
 */
export const RewardStatus = ({ status, error }: RewardStatusProps) => {
  // A failed load says nothing about the DAR or the delegation.
  if (error) {
    return (
      <Box sx={{ p: 2 }}>
        <Alert severity="error">Could not load the reward status: {error}</Alert>
      </Box>
    );
  }
  if (!status) {
    return (
      <Typography variant="body2" color="text.secondary" sx={{ p: 2 }}>
        Reading the reward status…
      </Typography>
    );
  }
  if (!status.rewards_dar_resolvable) {
    return (
      <Box sx={{ p: 2 }}>
        <Alert severity="warning">
          <AlertTitle>This participant does not have the rewards DAR</AlertTitle>
          Without governance-rewards-automation-v1, this node cannot reassign reward coupons.
          Upload that DAR to this participant, or use Check DARs to distribute it from a peer.
        </Alert>
      </Box>
    );
  }
  const { delegation } = status;
  if (!delegation) {
    return (
      <Typography variant="body2" color="text.secondary" sx={{ p: 2 }}>
        No reward delegation is active for this party, so no node reassigns its reward coupons. A
        Setup Coupon Reassignment Delegation vote creates one.
      </Typography>
    );
  }
  // One row per role, as in the Governance section. A party can hold both.
  const rows = [
    ...delegation.assigners.map((party) => ({ party, role: "Assigner", share: "" })),
    ...status.beneficiaries.map((b) => ({
      party: b.beneficiary,
      role: "Beneficiary",
      share: formatShare(b.percentage),
    })),
  ];
  return (
    <>
      <Box sx={{ p: 2 }}>
        {status.member_is_assigner ? (
          <Alert severity="success">
            This node is a listed assigner. It reassigns this party's reward coupons.
          </Alert>
        ) : (
          <Alert severity="info">
            This node is not a listed assigner, so it does not reassign this party's coupons. The
            listed assigners do.
          </Alert>
        )}
      </Box>
      <Box sx={{ overflowX: "auto", ...cardTableSx }}>
        <Table size="small" aria-label="Reward assigners and beneficiaries">
          <TableHead>
            <TableRow>
              <TableCell sx={{ py: 1 }}>Party</TableCell>
              <TableCell sx={{ py: 1 }}>Role</TableCell>
              <TableCell sx={{ py: 1 }} align="right">
                Share of each coupon
              </TableCell>
            </TableRow>
          </TableHead>
          <TableBody>
            {rows.map(({ party, role, share }, idx) => (
              <TableRow key={`${role}:${party}`} sx={zebraRow(idx)} data-testid="reward-party-row">
                <TableCell sx={{ py: 1 }}>
                  <Box sx={{ display: "flex", alignItems: "center", gap: 1 }}>
                    <CopyableText text={party} truncate={{ start: 32, end: 16 }} variant="body2" />
                    {party === status.member_party_id && <Chip label="This node" size="small" />}
                  </Box>
                </TableCell>
                <TableCell sx={{ py: 1 }}>{role}</TableCell>
                <TableCell sx={{ py: 1, fontFamily: "var(--font-mono)" }} align="right">
                  {share}
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </Box>
    </>
  );
};
