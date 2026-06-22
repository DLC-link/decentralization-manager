import { Box, IconButton, Tooltip, Typography, useMediaQuery, useTheme } from "@mui/material";
import ContentCopyIcon from "@mui/icons-material/ContentCopy";
import { useSnackbar } from "../contexts";
import { copyToClipboard } from "../clipboard";

interface CopyableTextProps {
  text: string;
  truncate?: {
    start: number;
    end: number;
  };
  variant?: "h6" | "body1" | "body2" | "caption";
}

export const CopyableText = ({ text, truncate, variant = "body1" }: CopyableTextProps) => {
  const { showSnackbar } = useSnackbar();
  const theme = useTheme();
  const isSmall = useMediaQuery(theme.breakpoints.down("sm"));
  const isMedium = useMediaQuery(theme.breakpoints.down("md"));

  const getDisplayText = () => {
    if (!truncate) return text;

    // Adjust truncation based on screen size
    let start = truncate.start;
    let end = truncate.end;

    if (isSmall) {
      start = Math.min(truncate.start, 8);
      end = Math.min(truncate.end, 6);
    } else if (isMedium) {
      start = Math.min(truncate.start, 16);
      end = Math.min(truncate.end, 10);
    }

    return `${text.slice(0, start)}...${text.slice(-end)}`;
  };

  const handleCopy = async (e: React.MouseEvent) => {
    e.stopPropagation();
    const success = await copyToClipboard(text);
    showSnackbar(success ? "Copied to clipboard" : "Failed to copy");
  };

  return (
    <Box sx={{ display: "flex", alignItems: "center", gap: 1 }}>
      <Tooltip title={text}>
        <Typography variant={variant} sx={{ fontFamily: "var(--font-mono)" }}>
          {getDisplayText()}
        </Typography>
      </Tooltip>
      <Tooltip title="Copy">
        <IconButton size="small" onClick={handleCopy}>
          <ContentCopyIcon fontSize="small" />
        </IconButton>
      </Tooltip>
    </Box>
  );
};
