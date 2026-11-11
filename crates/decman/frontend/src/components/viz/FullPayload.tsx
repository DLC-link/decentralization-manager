import { Box, IconButton, Tooltip, Typography } from "@mui/material";
import ContentCopyIcon from "@mui/icons-material/ContentCopy";
import { copyToClipboard } from "../../clipboard";
import { useSnackbar } from "../../contexts";

/**
 * The complete JSON of what an approval card confirms or executes, for the
 * card's Review panel. The summary above it picks out a few fields; this shows
 * every one, so nothing the member signs stays hidden.
 */
export const FullPayload = ({ value }: { value: unknown }) => {
  const { showSnackbar } = useSnackbar();
  const json = JSON.stringify(value, null, 2);
  return (
    <Box>
      <Box
        sx={{
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          mb: 0.5,
        }}
      >
        <Typography
          sx={{
            fontFamily: "var(--font-mono)",
            fontSize: 10,
            letterSpacing: "0.12em",
            textTransform: "uppercase",
            color: "text.secondary",
          }}
        >
          Full payload
        </Typography>
        <Tooltip title="Copy payload JSON">
          <IconButton
            size="small"
            aria-label="Copy payload JSON"
            onClick={async () => {
              const ok = await copyToClipboard(json);
              showSnackbar(ok ? "Copied to clipboard" : "Failed to copy");
            }}
            sx={{ p: 0.25 }}
          >
            <ContentCopyIcon sx={{ fontSize: 14 }} />
          </IconButton>
        </Tooltip>
      </Box>
      <Box
        component="pre"
        // Focusable so a keyboard user can scroll a payload taller than the box.
        tabIndex={0}
        role="region"
        aria-label="Full payload JSON"
        sx={{
          m: 0,
          px: 1.25,
          py: 1,
          maxHeight: 320,
          overflow: "auto",
          bgcolor: "action.hover",
          borderRadius: 1,
          fontFamily: "var(--font-mono)",
          fontSize: 12,
          lineHeight: 1.5,
          whiteSpace: "pre",
          "&:focus-visible": {
            outline: "2px solid",
            outlineColor: "primary.main",
          },
        }}
      >
        {json}
      </Box>
    </Box>
  );
};
