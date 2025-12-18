import { Box, IconButton, Tooltip, Typography } from "@mui/material";
import ContentCopyIcon from "@mui/icons-material/ContentCopy";
import { useSnackbar } from "../contexts";

export const copyToClipboard = async (text: string): Promise<boolean> => {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    // Fallback for non-HTTPS contexts
    try {
      const textArea = document.createElement("textarea");
      textArea.value = text;
      document.body.appendChild(textArea);
      textArea.select();
      document.execCommand("copy");
      document.body.removeChild(textArea);
      return true;
    } catch {
      return false;
    }
  }
};

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

  const displayText = truncate
    ? `${text.slice(0, truncate.start)}...${text.slice(-truncate.end)}`
    : text;

  const handleCopy = async () => {
    const success = await copyToClipboard(text);
    showSnackbar(success ? "Copied to clipboard" : "Failed to copy");
  };

  return (
    <Box sx={{ display: "flex", alignItems: "center", gap: 1 }}>
      <Tooltip title={text}>
        <Typography variant={variant} sx={{ fontFamily: "monospace" }}>
          {displayText}
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
