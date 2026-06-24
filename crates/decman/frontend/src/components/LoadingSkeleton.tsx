import {
  Box,
  Skeleton,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableRow,
} from "@mui/material";
import { zebraRow } from "../styles";

export const LoadingSkeleton = () => (
  <Table size="small">
    <TableHead>
      <TableRow>
        <TableCell sx={{ py: 1 }}><Skeleton width="60%" /></TableCell>
        <TableCell sx={{ py: 1 }} align="center"><Skeleton width={50} sx={{ mx: "auto" }} /></TableCell>
        <TableCell sx={{ py: 1 }} align="center"><Skeleton width={45} sx={{ mx: "auto" }} /></TableCell>
        <TableCell sx={{ py: 1 }} align="center"><Skeleton width={70} sx={{ mx: "auto" }} /></TableCell>
        <TableCell sx={{ py: 1 }} align="center"><Skeleton width={55} sx={{ mx: "auto" }} /></TableCell>
        <TableCell sx={{ py: 1 }} align="center"><Skeleton variant="circular" width={18} height={18} sx={{ mx: "auto" }} /></TableCell>
      </TableRow>
    </TableHead>
    <TableBody>
      {Array.from({ length: 20 }).map((_, i) => (
        <TableRow key={i} sx={zebraRow(i)}>
          <TableCell sx={{ py: 1.5 }}><Skeleton width={`${55 + (i % 3) * 10}%`} /></TableCell>
          <TableCell sx={{ py: 1.5 }} align="center"><Skeleton width={20} sx={{ mx: "auto" }} /></TableCell>
          <TableCell sx={{ py: 1.5 }} align="center"><Skeleton width={20} sx={{ mx: "auto" }} /></TableCell>
          <TableCell sx={{ py: 1.5 }} align="center"><Skeleton width={20} sx={{ mx: "auto" }} /></TableCell>
          <TableCell sx={{ py: 1.5 }} align="center"><Skeleton variant="rounded" width={32} height={20} sx={{ mx: "auto" }} /></TableCell>
          <TableCell sx={{ py: 1.5 }} align="center"><Skeleton variant="circular" width={18} height={18} sx={{ mx: "auto" }} /></TableCell>
        </TableRow>
      ))}
    </TableBody>
  </Table>
);

export const PackagesTabSkeleton = () => (
  <>
    <Box sx={{ display: "flex", justifyContent: "space-between", mb: 2 }}>
      <Skeleton variant="text" width="200px" height={20} />
      <Box sx={{ display: "flex", gap: 1 }}>
        <Skeleton variant="rounded" width={140} height={32} />
        <Skeleton variant="rounded" width={120} height={32} />
      </Box>
    </Box>
    {Array.from({ length: 8 }).map((_, i) => (
      <Box key={i} sx={{ display: "flex", gap: 2, py: 1.5, px: 1 }}>
        <Skeleton variant="text" width="35%" />
        <Skeleton variant="text" width="15%" />
        <Skeleton variant="text" width="40%" />
      </Box>
    ))}
  </>
);

export const ConfigTabSkeleton = () => (
  <>
    <Skeleton variant="text" width="60px" height={16} sx={{ mb: 1 }} />
    <Skeleton variant="text" width="50%" height={24} />
    <Skeleton variant="text" width="30%" height={20} />
    <Skeleton variant="text" width="30%" height={20} />
    <Skeleton variant="text" width="25%" height={20} />

    <Box sx={{ mt: 4 }}>
      <Skeleton variant="text" width="60px" height={24} sx={{ mb: 1 }} />
      {Array.from({ length: 3 }).map((_, i) => (
        <Box key={i} sx={{ display: "flex", gap: 2, py: 1.5 }}>
          <Skeleton variant="circular" width={12} height={12} sx={{ mt: 0.5 }} />
          <Skeleton variant="text" width="30%" />
          <Skeleton variant="text" width="15%" />
          <Skeleton variant="text" width="25%" />
        </Box>
      ))}
    </Box>
  </>
);
