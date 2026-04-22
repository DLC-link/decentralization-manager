import { Box, Card, Skeleton, Tabs, Tab, useMediaQuery, useTheme } from "@mui/material";

const PartyListSkeleton = () => (
  <Card sx={{ borderRadius: 2, overflow: "hidden" }}>
    {/* Header row */}
    <Box sx={{ display: "flex", gap: 2, py: 1.5, px: 2, bgcolor: "action.hover" }}>
      <Skeleton variant="text" width="40%" height={16} />
      <Skeleton variant="text" width="8%" height={16} />
      <Skeleton variant="text" width="8%" height={16} />
      <Skeleton variant="text" width="10%" height={16} />
      <Skeleton variant="text" width="10%" height={16} />
      <Skeleton variant="text" width="8%" height={16} />
    </Box>
    {Array.from({ length: 6 }).map((_, i) => (
      <Box key={i} sx={{ display: "flex", gap: 2, py: 1.5, px: 2 }}>
        <Skeleton variant="text" width="40%" height={20} />
        <Skeleton variant="text" width="8%" height={20} />
        <Skeleton variant="text" width="8%" height={20} />
        <Skeleton variant="text" width="10%" height={20} />
        <Skeleton variant="rounded" width={32} height={20} />
        <Skeleton variant="circular" width={20} height={20} />
      </Box>
    ))}
  </Card>
);

const TableRowSkeleton = () => (
  <Box sx={{ display: "flex", gap: 2, py: 1.5, px: 1 }}>
    <Skeleton variant="text" width="35%" />
    <Skeleton variant="text" width="15%" />
    <Skeleton variant="text" width="40%" />
  </Box>
);

export const LoadingSkeleton = () => {
  const muiTheme = useTheme();
  const isLargeScreen = useMediaQuery(muiTheme.breakpoints.up("lg"));

  return (
    <>
      {!isLargeScreen && (
        <Tabs value={0} sx={{ mb: 3, borderBottom: 1, borderColor: "divider" }}>
          <Tab label="Parties" disabled />
          <Tab label="Packages" disabled />
          <Tab label="Configuration" disabled />
        </Tabs>
      )}

      {isLargeScreen ? (
        <Box sx={{ height: 48 }} />
      ) : (
        <Box sx={{ mb: 3 }}>
          <Box sx={{ display: "flex", justifyContent: "space-between", mb: 2 }}>
            <Skeleton variant="text" width="140px" height={20} />
            <Skeleton variant="rounded" width={120} height={36} />
          </Box>
          <Skeleton variant="rounded" width={300} height={40} />
        </Box>
      )}

      <PartyListSkeleton />
    </>
  );
};

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
