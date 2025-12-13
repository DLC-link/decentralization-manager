import { Box, Card, CardContent, Skeleton } from "@mui/material";

const AccordionSkeleton = () => (
  <Card sx={{ mb: 2, borderRadius: 3 }}>
    <CardContent sx={{ p: 3 }}>
      <Skeleton variant="text" width="40%" height={32} />
      <Box sx={{ mt: 2 }}>
        <Skeleton variant="text" width="60%" />
        <Skeleton variant="text" width="50%" />
        <Skeleton variant="text" width="55%" />
        <Skeleton variant="text" width="45%" />
      </Box>
    </CardContent>
  </Card>
);

const PartyCardSkeleton = () => (
  <Card sx={{ mb: 3, borderRadius: 3 }}>
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

export const LoadingSkeleton = () => (
  <>
    <AccordionSkeleton />
    <AccordionSkeleton />

    <Box sx={{ mt: 5, mb: 3 }}>
      <Skeleton variant="text" width="200px" height={32} />
      <Skeleton variant="text" width="80px" height={20} />
    </Box>

    <PartyCardSkeleton />
    <PartyCardSkeleton />
  </>
);
