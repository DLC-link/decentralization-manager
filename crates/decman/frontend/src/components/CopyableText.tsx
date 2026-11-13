import { Box, IconButton, Tooltip, Typography, useMediaQuery, useTheme } from "@mui/material";
import ContentCopyIcon from "@mui/icons-material/ContentCopy";
import { useSnackbar } from "../contexts";
import { copyToClipboard } from "../clipboard";
import { shortenId } from "../cantonId";

interface CopyableTextProps {
  text: string;
  truncate?: {
    start: number;
    end: number;
  };
  variant?: "h6" | "body1" | "body2" | "caption";
  color?: string;
}

export const CopyableText = ({
  text,
  truncate,
  variant = "body1",
  color,
}: CopyableTextProps) => {
  const { showSnackbar } = useSnackbar();
  const theme = useTheme();
  const isSmall = useMediaQuery(theme.breakpoints.down("sm"));
  const isMedium = useMediaQuery(theme.breakpoints.down("md"));

  const getDisplayText = () => {
    if (!truncate) return text;

    let start = truncate.start;
    let end = truncate.end;

    if (isSmall) {
      start = Math.min(truncate.start, 8);
      end = Math.min(truncate.end, 6);
    } else if (isMedium) {
      start = Math.min(truncate.start, 16);
      end = Math.min(truncate.end, 10);
    }

    return shortenId(text, start, end);
  };

  const handleCopy = async (e: React.MouseEvent) => {
    e.stopPropagation();
    const success = await copyToClipboard(text);
    showSnackbar(success ? "Copied to clipboard" : "Failed to copy");
  };

  // One line, ellipsized when the cell is narrower than the text, so the copy
  // button always stays inside its own cell.
  return (
    <Box sx={{ display: "flex", alignItems: "center", gap: 1, minWidth: 0 }}>
      <Tooltip title={text}>
        <Typography
          variant={variant}
          sx={{
            fontFamily: "var(--font-mono)",
            minWidth: 0,
            overflow: "hidden",
            textOverflow: "ellipsis",
            whiteSpace: "nowrap",
            ...(color && { color }),
          }}
        >
          {getDisplayText()}
        </Typography>
      </Tooltip>
      <Tooltip title="Copy">
        <IconButton size="small" onClick={handleCopy} sx={{ flexShrink: 0 }}>
          <ContentCopyIcon fontSize="small" />
        </IconButton>
      </Tooltip>
    </Box>
  );
};
