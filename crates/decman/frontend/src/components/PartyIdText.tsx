import { Box, IconButton, Tooltip, Typography } from "@mui/material";
import ContentCopyIcon from "@mui/icons-material/ContentCopy";
import { useSnackbar } from "../contexts";
import { copyToClipboard } from "../clipboard";

/**
 * A party id split at `::` so the readable prefix carries the weight and the
 * namespace fingerprint recedes behind it.
 *
 * The namespace ellipsizes rather than middle-truncating at fixed breakpoints:
 * every namespace opens with the same multicodec bytes and diverges after, so
 * cutting the tail still leaves the distinguishing part on screen, and the row
 * shows as much as the viewport allows without media queries.
 */
export const PartyIdText = ({
  partyId,
  copyable = true,
}: {
  partyId: string;
  copyable?: boolean;
}) => {
  const { showSnackbar } = useSnackbar();
  const separator = partyId.indexOf("::");
  const prefix = separator === -1 ? partyId : partyId.slice(0, separator);
  const namespace = separator === -1 ? null : partyId.slice(separator + 2);

  const copy = async (e: React.MouseEvent) => {
    e.stopPropagation();
    const ok = await copyToClipboard(partyId);
    showSnackbar(ok ? "Copied to clipboard" : "Failed to copy");
  };

  return (
    <Box
      sx={{ display: "flex", alignItems: "center", gap: 1, flex: 1, minWidth: 0 }}
    >
      <Tooltip title={partyId}>
        <Box
          sx={{
            display: "flex",
            alignItems: "baseline",
            // Takes the whole id column rather than sizing to the text, so the
            // copy buttons line up down the list instead of following each id
            // to wherever it happens to end.
            flex: 1,
            minWidth: 0,
            fontFamily: "var(--font-mono)",
            fontSize: 14,
          }}
        >
          <Typography
            component="span"
            sx={{
              font: "inherit",
              fontWeight: 500,
              color: "text.primary",
              flexShrink: 0,
            }}
          >
            {prefix}
          </Typography>
          {namespace !== null && (
            <Typography
              component="span"
              sx={{
                font: "inherit",
                color: "text.disabled",
                minWidth: 0,
                overflow: "hidden",
                textOverflow: "ellipsis",
                whiteSpace: "nowrap",
              }}
            >
              ::{namespace}
            </Typography>
          )}
        </Box>
      </Tooltip>
      {copyable && (
        <Tooltip title="Copy party id">
          <IconButton
            size="small"
            aria-label="Copy party id"
            onClick={copy}
            sx={{ p: 0.25, flexShrink: 0 }}
          >
            <ContentCopyIcon sx={{ fontSize: 14 }} />
          </IconButton>
        </Tooltip>
      )}
    </Box>
  );
};
