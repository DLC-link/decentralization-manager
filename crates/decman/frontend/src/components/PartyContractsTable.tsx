import type { RefObject } from "react";
import { Box, Table, TableBody, TableCell, TableHead, TableRow } from "@mui/material";
import { CopyableText } from "./CopyableText";
import { PaginationControls } from "./Pagination";
import type { usePagination } from "../usePagination";
import { cardTableSx, zebraRow } from "../styles";
import type { ContractInfo } from "../types";

interface PartyContractsTableProps {
  canScrollUp: boolean;
  canScrollDown: boolean;
  contractsScrollRef: RefObject<HTMLDivElement | null>;
  contractsPaging: ReturnType<typeof usePagination<ContractInfo>>;
}

/** The Contracts section body: a scrolling, paged table of the party's contracts. */
const PartyContractsTable = ({
  canScrollUp,
  canScrollDown,
  contractsScrollRef,
  contractsPaging,
}: PartyContractsTableProps) => (
  <>
    <Box sx={{ position: "relative" }}>
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
        ref={contractsScrollRef}
        sx={{
          maxHeight: 180,
          overflowY: "auto",
          overflowX: "auto",
          ...cardTableSx,
        }}
      >
        <Table size="small">
          <TableHead>
            <TableRow>
              <TableCell sx={{ py: 1 }}>Package</TableCell>
              <TableCell sx={{ py: 1 }}>Version</TableCell>
              <TableCell sx={{ py: 1 }}>Template</TableCell>
              <TableCell sx={{ py: 1 }}>Created</TableCell>
              <TableCell sx={{ py: 1 }}>Contract ID</TableCell>
            </TableRow>
          </TableHead>
          <TableBody>
            {contractsPaging.pageItems.map((c, idx) => (
              <TableRow key={c.contract_id} sx={zebraRow(idx)}>
                <TableCell sx={{ py: 1 }}>
                  {c.package_name || "—"}
                </TableCell>
                <TableCell sx={{ py: 1 }}>
                  {c.package_version || "—"}
                </TableCell>
                <TableCell sx={{ py: 1 }}>{c.template_id}</TableCell>
                <TableCell sx={{ py: 1 }}>
                  {c.created_at
                    ? new Date(c.created_at).toLocaleString(undefined, {
                        year: "numeric",
                        month: "short",
                        day: "2-digit",
                        hour: "2-digit",
                        minute: "2-digit",
                      })
                    : "—"}
                </TableCell>
                <TableCell sx={{ py: 1 }}>
                  <CopyableText
                    text={c.contract_id}
                    truncate={{ start: 12, end: 12 }}
                    variant="caption"
                  />
                </TableCell>
              </TableRow>
            ))}
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
      page={contractsPaging.page}
      pageCount={contractsPaging.pageCount}
      total={contractsPaging.total}
      onChange={contractsPaging.setPage}
    />
  </>
);

export { PartyContractsTable };
