import { Box, IconButton, Tooltip, Typography } from "@mui/material";
import ContentCopyIcon from "@mui/icons-material/ContentCopy";
import { copyToClipboard } from "../../clipboard";
import { useSnackbar } from "../../contexts";
import { truncateKey } from "./format";

/** A labelled key fingerprint, shortened, with a button that copies it whole. */
const KeyRow = ({
  label,
  fingerprint,
  testId,
}: {
  label: string;
  fingerprint: string;
  testId: string;
}) => {
  const { showSnackbar } = useSnackbar();
  return (
    <Box
      sx={{ display: "flex", alignItems: "center", gap: 1 }}
      data-testid={testId}
    >
      <Typography variant="caption" color="text.secondary" sx={{ minWidth: 96 }}>
        {label}
      </Typography>
      <Typography variant="body2" sx={{ fontFamily: "var(--font-mono)" }}>
        {truncateKey(fingerprint)}
      </Typography>
      <Tooltip title={`Copy ${label.toLowerCase()}`}>
        <IconButton
          size="small"
          onClick={async () => {
            const ok = await copyToClipboard(fingerprint);
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

/**
 * The namespace owner key a kick removes. A member that owns the namespace
 * without hosting the party may have no participant, so the key is the one
 * name every kick card can show.
 */
const KickedOwnerKeyRow = ({ ownerKey }: { ownerKey: string }) => (
  <KeyRow label="Owner key" fingerprint={ownerKey} testId="kicked-owner-key" />
);

export { KeyRow, KickedOwnerKeyRow };
