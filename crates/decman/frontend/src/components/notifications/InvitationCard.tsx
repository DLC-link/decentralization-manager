import { useState } from "react";
import { Box, Button, Chip, CircularProgress, IconButton, Tooltip, Typography } from "@mui/material";
import ContentCopyIcon from "@mui/icons-material/ContentCopy";
import DownloadIcon from "@mui/icons-material/Download";
import { API_BASE } from "../../constants";
import { authenticatedFetch } from "../../api";
import { copyToClipboard } from "../../clipboard";
import { useSnackbar } from "../../contexts";
import { ApprovalCard } from "../viz/ApprovalCard";
import type { PendingInvitation } from "../../types";
import { formatRelativeTime, truncatePartyId, truncateKey } from "./format";
import { KickedOwnerKeyRow } from "./KickedOwnerKeyRow";

const InvitationCard = ({
  invitation,
  onAfter,
  onSelectParty,
}: {
  invitation: PendingInvitation;
  onAfter: () => void;
  onSelectParty: (partyId: string) => void;
}) => {
  const [busy, setBusy] = useState(false);
  const [readingDar, setReadingDar] = useState<number | null>(null);
  const { showSnackbar } = useSnackbar();

  // Pulled from the coordinator on demand, not carried by the invite: the
  // operator can read the DAR before deciding, and an invite that nobody
  // opens costs nothing.
  const readDar = async (index: number, filename: string) => {
    setReadingDar(index);
    try {
      const res = await authenticatedFetch(
        `${API_BASE}/invitations/${encodeURIComponent(invitation.id)}/dars/${index}`,
      );
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        throw new Error(data.error || `Could not read ${filename}`);
      }
      const url = URL.createObjectURL(await res.blob());
      const link = document.createElement("a");
      link.href = url;
      link.download = filename;
      link.click();
      URL.revokeObjectURL(url);
      if (res.headers.get("X-Decman-Dar-Unpinned") === "true") {
        showSnackbar(
          `Downloaded ${filename}, but the invitation pins no hash, so its content was not checked`,
          "error",
        );
      }
    } catch (err) {
      showSnackbar(
        err instanceof Error ? err.message : `Could not read ${filename}`,
        "error",
      );
    } finally {
      setReadingDar(null);
    }
  };

  const respond = async (path: "accept" | "decline") => {
    setBusy(true);
    try {
      const res = await authenticatedFetch(
        `${API_BASE}/invitations/${path}`,
        {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ id: invitation.id }),
        },
      );
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        throw new Error(data.error || `Failed to ${path}`);
      }
      showSnackbar(
        path === "accept"
          ? "Invitation accepted — workflow started"
          : "Invitation declined",
      );
      onAfter();
    } catch (err) {
      showSnackbar(
        err instanceof Error ? err.message : `Failed to ${path}`,
        "error",
      );
    } finally {
      setBusy(false);
    }
  };

  const fromLabel =
    invitation.coordinator_name ||
    `${invitation.coordinator_pubkey.slice(0, 12)}…${invitation.coordinator_pubkey.slice(-6)}`;
  // Render every detail the invite payload carried, regardless of workflow
  // type — mirrors WorkflowRunCard so the peer's invitation card is as rich
  // as the coordinator's run card.
  const showMeta =
    !!invitation.prefix ||
    !!invitation.dec_party_id ||
    !!invitation.kicked_participant ||
    !!invitation.kicked_owner_key ||
    !!invitation.new_participant ||
    invitation.new_threshold != null ||
    (invitation.participants?.length ?? 0) > 0 ||
    (invitation.package_names?.length ?? 0) > 0 ||
    (invitation.contract_intents?.length ?? 0) > 0 ||
    (invitation.dar_filenames?.length ?? 0) > 0;

  // The type is already in the eyebrow — make the title describe the target.
  const pkgCount = invitation.package_names?.length ?? 0;
  const darCount = invitation.dar_filenames?.length ?? 0;
  const inviteTitle = invitation.prefix
    ? `Join party ${invitation.prefix}`
    : invitation.kicked_participant
      ? `Remove ${truncatePartyId(invitation.kicked_participant)}`
      : invitation.kicked_owner_key
        ? `Remove owner key ${truncateKey(invitation.kicked_owner_key)}`
        : pkgCount > 0
        ? `Deploy ${pkgCount} package${pkgCount === 1 ? "" : "s"}`
        : darCount > 0
          ? `Distribute ${darCount} DAR${darCount === 1 ? "" : "s"}`
          : invitation.dec_party_id
            ? `On ${truncatePartyId(invitation.dec_party_id)}`
            : `${invitation.invitation_type} invitation`;

  return (
    <ApprovalCard
      accent
      dataAttrs={{
        "data-testid": "invitation-card",
        "data-kind": invitation.invitation_type,
      }}
      time={formatRelativeTime(invitation.received_at)}
      title={inviteTitle}
      facts={
        <Box
          sx={{
            display: "flex",
            flexWrap: "wrap",
            alignItems: "center",
            gap: "6px 16px",
            fontSize: 13,
            color: "text.secondary",
          }}
        >
          <Box sx={{ display: "flex", alignItems: "center", gap: 0.5 }}>
            from
            <Box
              component="span"
              sx={{
                fontFamily: "var(--font-mono)",
                fontSize: 12,
                color: "text.primary",
              }}
            >
              {fromLabel}
            </Box>
          </Box>
          {(invitation.participants?.length ?? 0) > 0 && (
            <Box component="span">
              {invitation.participants!.length} participant
              {invitation.participants!.length === 1 ? "" : "s"}
            </Box>
          )}
          {(invitation.package_names?.length ?? 0) > 0 && (
            <Box component="span">
              {invitation.package_names!.length} package
              {invitation.package_names!.length === 1 ? "" : "s"}
            </Box>
          )}
        </Box>
      }
      footerLeft={
        <Typography
          sx={{
            fontFamily: "var(--font-mono)",
            fontSize: 12,
            color: "text.disabled",
          }}
        >
          Coordinator is waiting on your acceptance
        </Typography>
      }
      actions={
        <>
          <Button
            variant="outlined"
            color="error"
            size="small"
            onClick={() => respond("decline")}
            disabled={busy}
          >
            Decline
          </Button>
          <Button
            variant="contained"
            color="success"
            size="small"
            onClick={() => respond("accept")}
            disabled={busy}
            startIcon={busy ? <CircularProgress size={14} /> : undefined}
          >
            Accept
          </Button>
        </>
      }
      detail={
        showMeta ? (
          <Box
            sx={{
              display: "flex",
              flexDirection: "column",
              gap: 0.75,
              px: 1.25,
              py: 1,
              bgcolor: "action.hover",
              borderRadius: 1,
            }}
          >
          {invitation.prefix && (
            <Box sx={{ display: "flex", alignItems: "baseline", gap: 1 }}>
              <Typography
                variant="caption"
                color="text.secondary"
                sx={{ minWidth: 96 }}
              >
                Prefix
              </Typography>
              <Typography variant="body2" sx={{ fontWeight: 600 }}>
                {invitation.prefix}
              </Typography>
            </Box>
          )}
          {invitation.dec_party_id && (
            <Box sx={{ display: "flex", alignItems: "baseline", gap: 1 }}>
              <Typography
                variant="caption"
                color="text.secondary"
                sx={{ minWidth: 96 }}
              >
                Dec party
              </Typography>
              <Typography
                component="span"
                variant="caption"
                onClick={() => onSelectParty(invitation.dec_party_id!)}
                sx={{
                  fontFamily: "var(--font-mono)",
                  color: "primary.main",
                  cursor: "pointer",
                  "&:hover": { textDecoration: "underline" },
                }}
              >
                {truncatePartyId(invitation.dec_party_id)}
              </Typography>
            </Box>
          )}
          {invitation.kicked_participant && (
            <Box sx={{ display: "flex", alignItems: "center", gap: 1 }}>
              <Typography
                variant="caption"
                color="text.secondary"
                sx={{ minWidth: 96 }}
              >
                Kicking
              </Typography>
              <Typography
                variant="body2"
                sx={{ fontWeight: 600, fontFamily: "var(--font-mono)" }}
              >
                {truncatePartyId(invitation.kicked_participant)}
              </Typography>
              <Tooltip title="Copy kicked party id">
                <IconButton
                  size="small"
                  onClick={async () => {
                    const ok = await copyToClipboard(invitation.kicked_participant!);
                    showSnackbar(ok ? "Copied to clipboard" : "Failed to copy");
                  }}
                  sx={{ p: 0.25 }}
                >
                  <ContentCopyIcon sx={{ fontSize: 14 }} />
                </IconButton>
              </Tooltip>
            </Box>
          )}
          {invitation.kicked_owner_key && (
            <KickedOwnerKeyRow ownerKey={invitation.kicked_owner_key} />
          )}
          {invitation.new_participant && (
            <Box sx={{ display: "flex", alignItems: "center", gap: 1 }}>
              <Typography
                variant="caption"
                color="text.secondary"
                sx={{ minWidth: 96 }}
              >
                Adding
              </Typography>
              <Typography
                variant="body2"
                sx={{ fontWeight: 600, fontFamily: "var(--font-mono)" }}
              >
                {truncatePartyId(invitation.new_participant)}
              </Typography>
              <Tooltip title="Copy added participant id">
                <IconButton
                  size="small"
                  onClick={async () => {
                    const ok = await copyToClipboard(invitation.new_participant!);
                    showSnackbar(ok ? "Copied to clipboard" : "Failed to copy");
                  }}
                  sx={{ p: 0.25 }}
                >
                  <ContentCopyIcon sx={{ fontSize: 14 }} />
                </IconButton>
              </Tooltip>
            </Box>
          )}
          {invitation.new_threshold != null && (
            <Box sx={{ display: "flex", alignItems: "baseline", gap: 1 }}>
              <Typography
                variant="caption"
                color="text.secondary"
                sx={{ minWidth: 96 }}
              >
                Threshold
              </Typography>
              <Typography variant="body2">
                {invitation.previous_threshold != null ? (
                  <>
                    <Box
                      component="span"
                      sx={{ color: "text.secondary" }}
                    >
                      {invitation.previous_threshold}
                    </Box>{" "}
                    →{" "}
                    <Box component="span" sx={{ fontWeight: 600 }}>
                      {invitation.new_threshold}
                    </Box>
                  </>
                ) : (
                  <Box component="span" sx={{ fontWeight: 600 }}>
                    {invitation.new_threshold}
                  </Box>
                )}
              </Typography>
            </Box>
          )}
          {invitation.confirmation_threshold != null && (
            <Box sx={{ display: "flex", alignItems: "baseline", gap: 1 }}>
              <Typography
                variant="caption"
                color="text.secondary"
                sx={{ minWidth: 96 }}
              >
                Confirmation
              </Typography>
              <Typography variant="body2">
                <Box component="span" sx={{ fontWeight: 600 }}>
                  {invitation.confirmation_threshold}
                </Box>
              </Typography>
            </Box>
          )}
          {invitation.participants && invitation.participants.length > 0 && (
            <Box sx={{ display: "flex", alignItems: "baseline", gap: 1 }}>
              <Typography
                variant="caption"
                color="text.secondary"
                sx={{ minWidth: 96 }}
              >
                Participants ({invitation.participants.length})
              </Typography>
              <Box sx={{ display: "flex", flexWrap: "wrap", gap: 0.5 }}>
                {invitation.participants.map((id) => (
                  <Chip
                    key={id}
                    size="small"
                    variant="outlined"
                    label={truncatePartyId(id)}
                  />
                ))}
              </Box>
            </Box>
          )}
          {invitation.package_names && invitation.package_names.length > 0 && (
            <Box sx={{ display: "flex", alignItems: "baseline", gap: 1 }}>
              <Typography
                variant="caption"
                color="text.secondary"
                sx={{ minWidth: 96 }}
              >
                Packages ({invitation.package_names.length})
              </Typography>
              <Box sx={{ display: "flex", flexWrap: "wrap", gap: 0.5 }}>
                {invitation.package_names.map((name) => (
                  <Chip key={name} size="small" variant="outlined" label={name} />
                ))}
              </Box>
            </Box>
          )}
          {(invitation.contract_intents?.length ?? 0) > 0 && (
            <Box sx={{ display: "flex", flexDirection: "column", gap: 1 }}>
              <Typography variant="caption" color="text.secondary">
                Approved creates — compare these commitments with your deployment configuration before accepting.
              </Typography>
              {invitation.contract_intents?.map((intent, index) => (
                <Box key={index} sx={{ overflowWrap: "anywhere" }}>
                  <Typography variant="caption" sx={{ display: "block" }}>
                    {intent.module_name}:{intent.entity_name}
                  </Typography>
                  <Typography variant="caption" sx={{ display: "block" }} color="text.secondary">
                    Package: {intent.package_id}
                  </Typography>
                  <Typography variant="caption" sx={{ display: "block" }} color="text.secondary">
                    Arguments SHA-256: {intent.argument_hash}
                  </Typography>
                </Box>
              ))}
            </Box>
          )}
          {invitation.dar_filenames && invitation.dar_filenames.length > 0 && (
            <Box sx={{ display: "flex", alignItems: "baseline", gap: 1 }}>
              <Typography
                variant="caption"
                color="text.secondary"
                sx={{ minWidth: 96 }}
              >
                DARs ({invitation.dar_filenames.length})
              </Typography>
              {/* The peer refuses any DAR whose bytes do not hash to the
                  value pinned here, so this is the operator accepting the
                  content rather than just the name. Shown in full so it can be
                  compared against a published release hash before accepting. */}
              <Box sx={{ display: "flex", flexDirection: "column", gap: 0.5 }}>
                {invitation.dar_filenames.map((filename, index) => {
                  const hash = invitation.dar_hashes?.[index];
                  return (
                    <Box
                      key={filename}
                      sx={{ display: "flex", alignItems: "baseline", gap: 1 }}
                    >
                      <Chip size="small" variant="outlined" label={filename} />
                      <Tooltip title={`Download ${filename} to read it before accepting`}>
                        <span>
                          <IconButton
                            size="small"
                            disabled={readingDar !== null}
                            onClick={() => readDar(index, filename)}
                            aria-label={`Download ${filename}`}
                          >
                            {readingDar === index ? (
                              <CircularProgress size={14} />
                            ) : (
                              <DownloadIcon fontSize="inherit" />
                            )}
                          </IconButton>
                        </span>
                      </Tooltip>
                      <Typography
                        variant="caption"
                        color={hash ? "text.secondary" : "warning.main"}
                        sx={{ fontFamily: "monospace", wordBreak: "break-all" }}
                      >
                        {hash ? `sha256:${hash}` : "no hash pinned"}
                      </Typography>
                    </Box>
                  );
                })}
              </Box>
            </Box>
          )}
          </Box>
        ) : undefined
      }
    />
  );
};

export { InvitationCard };
