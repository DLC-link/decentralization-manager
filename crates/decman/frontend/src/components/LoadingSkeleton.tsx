import { Box, Skeleton } from "@mui/material";
import { LIST_INSET, legendSx } from "../styles";
import { AUTH_SLOT, FAB_GUTTER, VISIBILITY_SLOT } from "./PartyList";
import { RowCard } from "./RowCard";

/**
 * Stand-in for the parties list. Built from the same row shell and column slots
 * as the real thing, so the rows don't resize or shift under the cursor when
 * the data lands.
 */
export const LoadingSkeleton = () => (
  <Box sx={{ px: LIST_INSET, pt: 1 }}>
    <Box
      sx={{ display: "flex", alignItems: "center", gap: 2, px: "16px", pb: 1 }}
    >
      <Box component="span" sx={{ ...legendSx, flex: 1, minWidth: 0 }}>
        Party ID
      </Box>
      <Box
        component="span"
        sx={{ ...legendSx, width: AUTH_SLOT, textAlign: "center", flexShrink: 0 }}
      >
        Auth
      </Box>
      <Box
        component="span"
        sx={{
          ...legendSx,
          width: VISIBILITY_SLOT,
          textAlign: "center",
          flexShrink: 0,
        }}
      >
        Visibility
      </Box>
      <Box sx={{ width: FAB_GUTTER, flexShrink: 0 }} aria-hidden />
    </Box>

    <Box sx={{ display: "flex", flexDirection: "column", gap: 1 }}>
      {Array.from({ length: 12 }).map((_, i) => (
        <RowCard key={i}>
          {/* Varied widths so the placeholder reads as a list of ids rather
            * than a stack of identical bars. The bar carries the width, not the
            * flex child — stretching it would flatten the variation back out. */}
          <Box sx={{ flex: 1, minWidth: 0 }}>
            <Skeleton width={`${38 + (i % 4) * 9}%`} />
          </Box>
          <Box
            sx={{
              width: AUTH_SLOT,
              flexShrink: 0,
              display: "flex",
              justifyContent: "center",
              minHeight: 30,
              alignItems: "center",
            }}
          >
            <Skeleton variant="circular" width={18} height={18} />
          </Box>
          <Box
            sx={{
              width: VISIBILITY_SLOT,
              flexShrink: 0,
              display: "flex",
              justifyContent: "center",
            }}
          >
            <Skeleton variant="circular" width={18} height={18} />
          </Box>
          <Box sx={{ width: FAB_GUTTER, flexShrink: 0 }} aria-hidden />
        </RowCard>
      ))}
    </Box>
  </Box>
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
