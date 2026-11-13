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
import { sameNamespace } from "../cantonId";
import type { GovernanceState } from "../types";

interface GovernanceRulesProps {
  state: GovernanceState;
  /** This node's member party; its row is marked when it governs. */
  memberPartyId?: string;
  /** This node's participant id. A party under its namespace is this node's. */
  selfParticipantId?: string;
  /** The party whose page shows these rules; its own id is not repeated. */
  partyId?: string;
}

const Fact = ({ label, children }: { label: string; children: ReactNode }) => (
  <Box sx={{ display: "flex", alignItems: "center", gap: 1.5, minWidth: 0 }}>
    <Typography component="span" sx={{ ...legendSx, flexShrink: 0 }}>
      {label}
    </Typography>
    {children}
  </Box>
);

/**
 * The active GovernanceRules contract: who governs the party, who else may
 * propose an action, and who else may see the contract. The stat cards above
 * carry the threshold and the action timeout.
 */
export const GovernanceRules = ({
  state,
  memberPartyId,
  selfParticipantId,
  partyId,
}: GovernanceRulesProps) => {
  const isThisNode = (party: string) =>
    party === memberPartyId || (!!selfParticipantId && sameNamespace(party, selfParticipantId));
  const rows = [
    ...state.members.map((party) => ({ party, role: "Member" })),
    ...state.additional_proposers.map((party) => ({ party, role: "Additional proposer" })),
    ...state.disclosure_observers.map((party) => ({ party, role: "Disclosure observer" })),
  ];
  return (
    <>
      <Box
        sx={{
          display: "flex",
          alignItems: "center",
          flexWrap: "wrap",
          columnGap: 4,
          rowGap: 1,
          // The same inset as the table's first column (cardTableSx).
          px: "16px",
          py: 1.25,
        }}
      >
        {/* An empty member set is a failed read (getGovernanceMembership
            treats it so), not a party nobody governs. */}
        {state.members.length === 0 && (
          <Typography variant="body2" color="warning.main" data-testid="governance-members-unread">
            The member set could not be read
          </Typography>
        )}
        {state.governance_party !== partyId && (
          <Fact label="Governance party">
            <CopyableText text={state.governance_party} truncate={{ start: 20, end: 12 }} variant="body2" />
          </Fact>
        )}
        <Fact label="Rules contract">
          <CopyableText text={state.contract_id} truncate={{ start: 12, end: 8 }} variant="body2" />
          {state.package_ref && (
            <Typography variant="body2" color="text.secondary" sx={{ whiteSpace: "nowrap" }}>
              {state.package_ref}
              {state.out_of_date ? " (older package)" : ""}
            </Typography>
          )}
        </Fact>
      </Box>
      <Box sx={{ overflowX: "auto", ...cardTableSx }}>
        <Table size="small" aria-label="Governance members, proposers and observers">
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
                    {isThisNode(party) && <Chip label="This node" size="small" />}
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
            {state.disclosure_observers.length === 0 && (
              <TableRow>
                <TableCell colSpan={2} sx={{ py: 1 }}>
                  <Typography variant="body2" color="text.secondary">
                    No disclosure observers: only nodes that host the governance party can see these rules.
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
