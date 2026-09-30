import type { ReactNode } from "react";
import {
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
import { cardTableSx, legendSx, zebraRow } from "../styles";
import { formatMicroseconds } from "../governanceFormat";
import type { GovernanceState } from "../types";

interface GovernanceRulesProps {
  state: GovernanceState;
  /** This node's member party; its row is marked when it governs. */
  memberPartyId?: string;
}

const Fact = ({ label, children }: { label: string; children: ReactNode }) => (
  <Box sx={{ minWidth: 0 }}>
    <Typography component="div" sx={legendSx}>
      {label}
    </Typography>
    <Box sx={{ mt: 0.5 }}>{children}</Box>
  </Box>
);

/**
 * The active GovernanceRules contract: who governs the party, how many of
 * them must confirm an action, and who else may propose one.
 */
export const GovernanceRules = ({ state, memberPartyId }: GovernanceRulesProps) => {
  const rows = [
    ...state.members.map((party) => ({ party, role: "Member" })),
    ...state.additional_proposers.map((party) => ({ party, role: "Additional proposer" })),
  ];
  return (
    <>
      <Box
        sx={{
          display: "grid",
          gridTemplateColumns: "repeat(auto-fit, minmax(220px, 1fr))",
          gap: 2,
          // The same inset as the table's first column (cardTableSx).
          px: "16px",
          pb: 2,
        }}
      >
        <Fact label="Threshold">
          <Typography variant="body2" data-testid="governance-threshold">
            {state.threshold} of {state.members.length} member
            {state.members.length === 1 ? "" : "s"} must confirm
          </Typography>
        </Fact>
        {state.action_confirmation_timeout_microseconds != null && (
          <Fact label="Confirmations expire after">
            <Typography variant="body2">
              {formatMicroseconds(state.action_confirmation_timeout_microseconds)}
            </Typography>
          </Fact>
        )}
        <Fact label="Governance party">
          <CopyableText text={state.governance_party} truncate={{ start: 20, end: 12 }} variant="body2" />
        </Fact>
        <Fact label="Rules contract">
          <CopyableText text={state.contract_id} truncate={{ start: 12, end: 8 }} variant="body2" />
          {state.package_ref && (
            <Typography variant="caption" color="text.secondary" sx={{ display: "block" }}>
              {state.package_ref}
              {state.out_of_date ? " (older package)" : ""}
            </Typography>
          )}
        </Fact>
      </Box>
      <Box sx={{ overflowX: "auto", ...cardTableSx }}>
        <Table size="small" aria-label="Governance members and proposers">
          <TableHead>
            <TableRow>
              <TableCell sx={{ py: 1 }}>Party</TableCell>
              <TableCell sx={{ py: 1 }}>Role</TableCell>
            </TableRow>
          </TableHead>
          <TableBody>
            {rows.map(({ party, role }, idx) => (
              <TableRow key={`${role}:${party}`} sx={zebraRow(idx)} data-testid="governance-party-row">
                <TableCell sx={{ py: 1 }}>
                  <Box sx={{ display: "flex", alignItems: "center", gap: 1 }}>
                    <CopyableText text={party} truncate={{ start: 32, end: 16 }} variant="body2" />
                    {party === memberPartyId && <Chip label="This node" size="small" />}
                  </Box>
                </TableCell>
                <TableCell sx={{ py: 1 }}>{role}</TableCell>
              </TableRow>
            ))}
            {state.additional_proposers.length === 0 && (
              <TableRow>
                <TableCell colSpan={2} sx={{ py: 1 }}>
                  <Typography variant="body2" color="text.secondary">
                    No additional proposers: only members can propose actions.
                  </Typography>
                </TableCell>
              </TableRow>
            )}
          </TableBody>
        </Table>
      </Box>
    </>
  );
};
