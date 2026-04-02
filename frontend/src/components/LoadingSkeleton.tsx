import { Box, Card, CardContent, Skeleton, Tabs, Tab } from "@mui/material";

const PartyCardSkeleton = () => (
  <Card sx={{ mb: 3, borderRadius: 2 }}>
    <CardContent sx={{ p: 3 }}>
      <Skeleton variant="text" width="70%" height={32} />
      <Box sx={{ display: "flex", gap: 1, mt: 1.5, mb: 2 }}>
        <Skeleton variant="rounded" width={100} height={24} />
        <Skeleton variant="rounded" width={80} height={24} />
        <Skeleton variant="rounded" width={110} height={24} />
        <Skeleton variant="rounded" width={90} height={24} />
      </Box>
      <Skeleton variant="text" width="30%" height={20} sx={{ mt: 3 }} />
      <Box sx={{ mt: 1.5 }}>
        <Skeleton variant="rounded" width="100%" height={40} />
        <Skeleton variant="rounded" width="100%" height={40} sx={{ mt: 1 }} />
        <Skeleton variant="rounded" width="100%" height={40} sx={{ mt: 1 }} />
      </Box>
    </CardContent>
  </Card>
);

const TableRowSkeleton = () => (
  <Box sx={{ display: "flex", gap: 2, py: 1.5, px: 1 }}>
    <Skeleton variant="text" width="35%" />
    <Skeleton variant="text" width="15%" />
    <Skeleton variant="text" width="40%" />
  </Box>
);

export const LoadingSkeleton = () => (
  <>
    <Tabs value={0} sx={{ mb: 3, borderBottom: 1, borderColor: "divider" }}>
      <Tab label="Parties" disabled />
      <Tab label="Packages" disabled />
      <Tab label="Configuration" disabled />
    </Tabs>

    <Box sx={{ mb: 3 }}>
      <Box sx={{ display: "flex", justifyContent: "space-between", mb: 2 }}>
        <Skeleton variant="text" width="140px" height={20} />
        <Skeleton variant="rounded" width={120} height={36} />
      </Box>
      <Skeleton variant="rounded" width={300} height={40} />
    </Box>

    <PartyCardSkeleton />
    <PartyCardSkeleton />
  </>
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
      <TableRowSkeleton key={i} />
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
