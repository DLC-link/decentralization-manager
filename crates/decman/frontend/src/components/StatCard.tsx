import type { ReactNode } from "react";
import { Box, Typography } from "@mui/material";
import { TextHelp } from "./FieldHelp";
import { sectionCardSx } from "../styles";

/**
 * One figure in the dashboard strip: a tracked-out label over the value it
 * measures. A pill wedged between the action buttons is what these used to be —
 * too small to read at a glance, which is the whole point of a summary.
 */
export const StatCard = ({
  label,
  value,
  unit,
  helpText,
  testId,
}: {
  label: string;
  value: ReactNode;
  /// Denominator or suffix, set beside the value at a quieter weight — "2 / 3"
  /// says more about a threshold than "2" does.
  unit?: string;
  /// Optional plain-English explanation, on the label.
  helpText?: string;
  /// Optional data-testid placed on the value node for e2e selectors.
  testId?: string;
}) => {
  const labelNode = (
    <Typography
      component="span"
      sx={{
        fontFamily: "var(--font-mono)",
        fontSize: "0.65rem",
        fontWeight: 500,
        letterSpacing: "0.12em",
        textTransform: "uppercase",
        color: "text.secondary",
        whiteSpace: "nowrap",
      }}
    >
      {label}
    </Typography>
  );
  return (
    <Box sx={{ ...sectionCardSx, px: 2, py: 1.25, minWidth: 128, flex: "1 1 0" }}>
      {helpText ? <TextHelp text={helpText}>{labelNode}</TextHelp> : labelNode}
      <Box sx={{ display: "flex", alignItems: "baseline", gap: 0.75, mt: 0.5 }}>
        <Typography
          component="span"
          sx={{ fontSize: 22, fontWeight: 600, lineHeight: 1.1 }}
          data-testid={testId}
        >
          {value}
        </Typography>
        {unit && (
          <Typography
            component="span"
            sx={{
              fontFamily: "var(--font-mono)",
              fontSize: 13,
              color: "text.secondary",
            }}
          >
            {unit}
          </Typography>
        )}
      </Box>
    </Box>
  );
};
