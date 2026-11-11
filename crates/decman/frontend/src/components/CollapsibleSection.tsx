import type { ReactNode } from "react";
import { Box, Collapse, Typography } from "@mui/material";
import ExpandMoreIcon from "@mui/icons-material/ExpandMore";
import { TextHelp } from "./FieldHelp";
import { SURFACE2, sectionCardSx } from "../styles";

interface CollapsibleSectionProps {
  title: string;
  expanded: boolean;
  onToggle: () => void;
  badge?: ReactNode;
  /// Optional plain-English explanation of the section's title word.
  /// Wraps the title in a `TextHelp` so hovering / focusing the title
  /// reveals a tooltip — no inline icon, since the title text itself is
  /// the trigger.
  helpText?: string;
  children: ReactNode;
}

/**
 * Section titles carry more weight than the generic `subtitle2` eyebrow, and sit
 * at full text contrast against the secondary grey of the table head below.
 */
const sectionTitleSx = { fontWeight: 700, color: "text.primary" };

/**
 * One section of the dashboard: a card whose header is the control that opens
 * it. These were edge-to-edge grey bands stacked on the page, so five of them
 * read as one undifferentiated wall and nothing told the eye where a section
 * ended. The card's own edge does that now, and it bounds the table inside so
 * the row rules stop with it.
 */
const CollapsibleSection = ({
  title,
  expanded,
  onToggle,
  badge,
  helpText,
  children,
}: CollapsibleSectionProps) => (
  <Box sx={sectionCardSx}>
    <Box
      sx={(theme) => ({
        display: "flex",
        alignItems: "center",
        cursor: "pointer",
        px: 2,
        py: 1.25,
        // Only while open, so a shut card reads as one quiet strip and an open
        // one gets a header its table sits under.
        borderBottom: expanded ? "1px solid" : "none",
        borderColor: "divider",
        backgroundColor: expanded ? SURFACE2[theme.palette.mode] : "transparent",
        "&:hover": {
          backgroundColor:
            theme.palette.mode === "light"
              ? "rgba(0, 0, 0, 0.03)"
              : "rgba(255, 255, 255, 0.04)",
        },
        transition: "background-color 0.2s ease",
      })}
      onClick={onToggle}
    >
      <ExpandMoreIcon
        fontSize="small"
        sx={{
          mr: 1,
          color: "text.secondary",
          transform: expanded ? "rotate(180deg)" : "rotate(0deg)",
          transition: "transform 0.2s ease",
        }}
      />
      {helpText ? (
        <TextHelp text={helpText}>
          <Typography variant="subtitle2" component="span" sx={sectionTitleSx}>
            {title}
          </Typography>
        </TextHelp>
      ) : (
        <Typography variant="subtitle2" sx={sectionTitleSx}>
          {title}
        </Typography>
      )}
      {badge}
    </Box>
    <Collapse in={expanded}>{children}</Collapse>
  </Box>
);

export { CollapsibleSection };
