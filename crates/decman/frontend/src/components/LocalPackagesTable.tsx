import { Skeleton, Table, TableHead, TableBody, TableRow, TableCell } from "@mui/material";
import { CopyableText } from "./CopyableText";
import { finderTableSx, zebraRow } from "../styles";
import type { VettedPackageInfo } from "../types";
import { VERSION_COL_WIDTH, PACKAGE_ID_COL_WIDTH, LOCAL_TABLE_MIN_WIDTH } from "./packageColumns";
import { VersionCell } from "./VersionCell";

const localColumns = (
  <colgroup>
    <col />
    <col style={{ width: VERSION_COL_WIDTH }} />
    <col style={{ width: PACKAGE_ID_COL_WIDTH }} />
  </colgroup>
);

const LocalPackagesSkeleton = () => (
  <Table
    size="small"
    stickyHeader
    sx={{ minWidth: LOCAL_TABLE_MIN_WIDTH, tableLayout: "fixed", ...finderTableSx }}
  >
    {localColumns}
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
);

interface LocalPackagesTableProps {
  localPaging: { pageItems: VettedPackageInfo[] };
}

const LocalPackagesTable = ({ localPaging }: LocalPackagesTableProps) => (
  <Table
    size="small"
    stickyHeader
    sx={{ minWidth: LOCAL_TABLE_MIN_WIDTH, tableLayout: "fixed", ...finderTableSx }}
  >
    {localColumns}
    <TableHead>
      <TableRow>
        <TableCell sx={{ py: 1 }}>Package Name</TableCell>
        <TableCell sx={{ py: 1 }}>Version</TableCell>
        <TableCell sx={{ py: 1 }}>Package ID</TableCell>
      </TableRow>
    </TableHead>
    <TableBody>
      {localPaging.pageItems.map((p, idx) => (
        <TableRow key={p.package_id} sx={zebraRow(idx)}>
          <TableCell sx={{ py: 1, overflowWrap: "anywhere" }}>
            {p.package_name || "-"}
          </TableCell>
          <VersionCell version={p.package_version} />
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
);

export { LocalPackagesSkeleton, LocalPackagesTable };
