import { useState } from "react";
import {
  Alert,
  Box,
  Button,
  Chip,
  CircularProgress,
  IconButton,
  Tooltip,
  Typography,
  type ChipProps,
} from "@mui/material";
import CheckCircleIcon from "@mui/icons-material/CheckCircle";
import RefreshIcon from "@mui/icons-material/Refresh";
import WarningIcon from "@mui/icons-material/Warning";
import { CopyableText } from "./CopyableText";
import { NodeIdentityDialog } from "./NodeIdentityDialog";
import { useSnackbar } from "../contexts";
import type {
  NodeIdentityPeer,
  NodeIdentityResponse,
  NodeIdentityStatusResponse,
  NodeLoginStatus,
  NodeLoginSummary,
} from "../types";

interface NodeIdentityCardProps {
  /** `null` until the first `/node-identity` fetch lands. */
  status: NodeIdentityStatusResponse | null;
  error: string | null;
  loading: boolean;
  onRefresh: () => void;
  /** Insecure mode: setup takes no login. */
  testMode?: boolean;
}

const LABEL_SX = { fontSize: "0.78rem", color: "text.secondary" } as const;
const VALUE_SX = {
  fontFamily: "var(--font-mono)",
  fontSize: "0.8rem",
  overflowWrap: "anywhere",
} as const;

const Row = ({ label, children }: { label: string; children: React.ReactNode }) => (
  <Box sx={{ display: "flex", flexDirection: "column", gap: 0.25, minWidth: 0 }}>
    <Typography sx={LABEL_SX}>{label}</Typography>
    {children}
  </Box>
);

const TOKEN_CHIP: Record<
  NodeLoginStatus["status"],
  { label: string; color: ChipProps["color"] }
> = {
  ok: { label: "login ok", color: "success" },
  mock: { label: "unsafe token", color: "warning" },
  failed: { label: "login failed", color: "error" },
};

const describeLogin = (login: NodeLoginSummary | undefined): string => {
  if (!login) return "Unsafe token (insecure mode)";
  if (login.idp === "auth0") {
    return `Auth0 · ${login.client_id} · ${login.auth0_domain} · ${login.auth0_audience}`;
  }
  return `Keycloak · ${login.client_id} · ${login.keycloak_url} · ${login.keycloak_realm}`;
};

const describePeer = (peer: NodeIdentityPeer): string => {
  if (peer.verified) return "verified";
  if (!peer.found) return "no node party";
  if (!peer.namespace_matches) return "wrong namespace";
  return peer.hosting_permission ? `hosted with ${peer.hosting_permission}` : "not hosted";
};

/**
 * The Config tab's Node identity block: the party this node acts as, the
 * login that acts as it, and whether each peer's node party checks out.
 */
export const NodeIdentityCard = ({
  status,
  error,
  loading,
  onRefresh,
  testMode = false,
}: NodeIdentityCardProps) => {
  const { showSnackbar } = useSnackbar();
  const [dialogOpen, setDialogOpen] = useState(false);
  const identity = status?.identity;
  const token = identity?.token;

  const handleSaved = (response: NodeIdentityResponse) => {
    const granted = response.rights_granted ? " and granted it act-as" : "";
    showSnackbar(`Node identity saved for user ${response.user_id}${granted}`);
    onRefresh();
  };

  return (
    <Box>
      <Box
        sx={{
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          flexWrap: "wrap",
          gap: 1.5,
          pb: 1.75,
          borderBottom: "1px solid",
          borderColor: "divider",
        }}
      >
        <Box sx={{ display: "flex", alignItems: "center", gap: 1.5, flexWrap: "wrap" }}>
          <Typography variant="subtitle2" color="text.secondary">
            Node identity
          </Typography>
          {token && (
            <Chip
              size="small"
              color={TOKEN_CHIP[token.status].color}
              label={TOKEN_CHIP[token.status].label}
              sx={{ height: 20, fontFamily: "var(--font-mono)", fontSize: "0.68rem" }}
            />
          )}
          {status && !identity && (
            <Chip
              size="small"
              label="not set up"
              sx={{ height: 20, fontFamily: "var(--font-mono)", fontSize: "0.68rem" }}
            />
          )}
        </Box>
        <Box sx={{ display: "flex", alignItems: "center", gap: 1 }}>
          <Tooltip title="Check the node identity again">
            <span>
              <IconButton
                size="small"
                aria-label="Refresh node identity"
                onClick={onRefresh}
                disabled={loading}
              >
                {loading ? <CircularProgress size={16} /> : <RefreshIcon fontSize="small" />}
              </IconButton>
            </span>
          </Tooltip>
          <Button size="small" variant="outlined" onClick={() => setDialogOpen(true)}>
            {!identity ? "Set up" : testMode ? "Set up again" : "Change login"}
          </Button>
        </Box>
      </Box>

      <Box sx={{ display: "flex", flexDirection: "column", gap: 1.5, pt: 2 }}>
        {error && <Alert severity="error">{error}</Alert>}
        {token?.status === "failed" && <Alert severity="error">{token.error}</Alert>}
        {identity && !identity.participant_matches && (
          <Alert severity="warning">
            The stored identity was set up on {identity.participant_id}, not on this
            node&apos;s participant. Set it up again.
          </Alert>
        )}
        {status && !identity && (
          <Typography variant="body2" color="text.secondary">
            Not set up. Set it up to give this node its own party and Ledger API login.
          </Typography>
        )}

        {identity && (
          <>
            <Row label="Node party">
              <CopyableText
                text={identity.node_party_id}
                truncate={{ start: 24, end: 8 }}
                variant="body2"
              />
            </Row>
            <Row label="Ledger API user">
              <Typography sx={VALUE_SX}>{identity.user_id}</Typography>
            </Row>
            <Row label="Login">
              <Typography sx={VALUE_SX}>{describeLogin(identity.login)}</Typography>
            </Row>
            <Row label="Saved">
              <Typography sx={VALUE_SX}>
                {new Date(identity.updated_at * 1000).toLocaleString()}
              </Typography>
            </Row>
          </>
        )}

        {status && (
          <Row label="Node parties in this node's topology">
            {[{ ...status.self, name: status.self.name || "this node" }, ...status.peers].map(
              (peer) => (
                <Box
                  key={peer.participant_id}
                  sx={{ display: "flex", alignItems: "center", gap: 1, minWidth: 0 }}
                >
                  {peer.verified ? (
                    <CheckCircleIcon color="success" sx={{ fontSize: 16 }} />
                  ) : (
                    <WarningIcon color="warning" sx={{ fontSize: 16 }} />
                  )}
                  <Typography sx={{ fontSize: "0.85rem" }}>{peer.name}</Typography>
                  <Typography sx={{ ...VALUE_SX, color: "text.secondary" }}>
                    {describePeer(peer)}
                  </Typography>
                </Box>
              ),
            )}
          </Row>
        )}
      </Box>

      <NodeIdentityDialog
        open={dialogOpen}
        onClose={() => setDialogOpen(false)}
        onSaved={handleSaved}
        login={identity?.login}
        testMode={testMode}
      />
    </Box>
  );
};
