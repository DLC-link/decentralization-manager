import { Box, IconButton, Tooltip, Typography } from "@mui/material";
import ContentCopyIcon from "@mui/icons-material/ContentCopy";
import { copyToClipboard } from "../../clipboard";
import { useSnackbar } from "../../contexts";
import { truncateKey } from "./format";

/**
 * The namespace owner key a kick removes. A member that owns the namespace
 * without hosting the party may have no participant, so the key is the one
 * name every kick card can show.
 */
const KickedOwnerKeyRow = ({ ownerKey }: { ownerKey: string }) => {
  const { showSnackbar } = useSnackbar();
  return (
    <Box
      sx={{ display: "flex", alignItems: "center", gap: 1 }}
      data-testid="kicked-owner-key"
    >
      <Typography variant="caption" color="text.secondary" sx={{ minWidth: 96 }}>
        Owner key
      </Typography>
      <Typography variant="body2" sx={{ fontFamily: "var(--font-mono)" }}>
        {truncateKey(ownerKey)}
      </Typography>
      <Tooltip title="Copy owner key">
        <IconButton
          size="small"
          onClick={async () => {
            const ok = await copyToClipboard(ownerKey);
            showSnackbar(ok ? "Copied to clipboard" : "Failed to copy");
          }}
          sx={{ p: 0.25 }}
        >
          <ContentCopyIcon sx={{ fontSize: 14 }} />
        </IconButton>
      </Tooltip>
    </Box>
  );
};

export { KickedOwnerKeyRow };
