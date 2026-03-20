import { useState, useEffect, useRef, useCallback } from "react";
import {
  Dialog,
  DialogTitle,
  DialogContent,
  DialogActions,
  Button,
  TextField,
  Box,
  Typography,
  CircularProgress,
  Alert,
  Divider,
  ToggleButtonGroup,
  ToggleButton,
} from "@mui/material";
import { API_BASE } from "../constants";
import type {
  PartyConfigResponse,
  PartyConfigRequest,
  PackageConfig,
} from "../types";

type AuthMethod = "client_credentials" | "password";

interface PartyConfigDialogProps {
  open: boolean;
  onClose: () => void;
  onSave: () => void;
  partyId: string;
}

export const PartyConfigDialog = ({
  open,
  onClose,
  onSave,
  partyId,
}: PartyConfigDialogProps) => {
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState(false);

  const [memberPartyId, setMemberPartyId] = useState("");
  const [userId, setUserId] = useState("");
  const [keycloakUrl, setKeycloakUrl] = useState("");
  const [keycloakRealm, setKeycloakRealm] = useState("");
  const [keycloakClientId, setKeycloakClientId] = useState("");
  const [keycloakClientSecret, setKeycloakClientSecret] = useState("");
  const [keycloakUsername, setKeycloakUsername] = useState("");
  const [keycloakPassword, setKeycloakPassword] = useState("");
  const [authMethod, setAuthMethod] =
    useState<AuthMethod>("client_credentials");
  const [packages, setPackages] = useState<PackageConfig>({});

  const [canScrollUp, setCanScrollUp] = useState(false);
  const [canScrollDown, setCanScrollDown] = useState(false);
  const scrollRef = useRef<HTMLDivElement>(null);

  const updateScrollShadows = useCallback(() => {
    const el = scrollRef.current;
    if (el) {
      setCanScrollUp(el.scrollTop > 0);
      setCanScrollDown(
        el.scrollHeight - el.clientHeight - el.scrollTop > 2,
      );
    }
  }, []);

  useEffect(() => {
    if (!open || loading) return;
    const el = scrollRef.current;
    if (!el) return;

    updateScrollShadows();
    el.addEventListener("scroll", updateScrollShadows);

    const observer = new ResizeObserver(updateScrollShadows);
    observer.observe(el);

    return () => {
      el.removeEventListener("scroll", updateScrollShadows);
      observer.disconnect();
    };
  }, [open, loading, updateScrollShadows]);

  useEffect(() => {
    if (open) {
      setError(null);
      setSuccess(false);
      fetchConfig();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, partyId]);

  const fetchConfig = async () => {
    setLoading(true);
    try {
      const res = await fetch(
        `${API_BASE}/party-config/${encodeURIComponent(partyId)}`,
      );
      if (res.ok) {
        const data: PartyConfigResponse = await res.json();
        setMemberPartyId(data.member_party_id);
        setUserId(data.user_id);
        setKeycloakUrl(data.keycloak_url);
        setKeycloakRealm(data.keycloak_realm);
        setKeycloakClientId(data.keycloak_client_id);
        setPackages(data.packages);
        setKeycloakClientSecret("");
        setKeycloakUsername("");
        setKeycloakPassword("");
        setAuthMethod(
          data.has_username || data.has_password
            ? "password"
            : "client_credentials",
        );
      }
    } catch {
      // Config not found, fields stay empty
    } finally {
      setLoading(false);
    }
  };

  const handleSave = async () => {
    setSaving(true);
    setError(null);
    setSuccess(false);

    const body: PartyConfigRequest = {
      dec_party_id: partyId,
      member_party_id: memberPartyId,
      user_id: userId,
      keycloak_url: keycloakUrl,
      keycloak_realm: keycloakRealm,
      keycloak_client_id:
        authMethod === "client_credentials" ? keycloakClientId : "",
      packages,
    };

    if (authMethod === "client_credentials") {
      body.keycloak_client_secret = keycloakClientSecret || undefined;
      body.keycloak_username = "";
      body.keycloak_password = "";
    } else {
      body.keycloak_client_secret = "";
      body.keycloak_username = keycloakUsername || undefined;
      body.keycloak_password = keycloakPassword || undefined;
    }

    try {
      const res = await fetch(`${API_BASE}/party-config`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      });

      if (!res.ok) {
        const data = await res.json();
        throw new Error(data.error || "Failed to save configuration");
      }

      setSuccess(true);
      onSave();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Unknown error");
    } finally {
      setSaving(false);
    }
  };

  const handleClose = () => {
    if (!saving) {
      onClose();
    }
  };

  return (
    <Dialog open={open} onClose={handleClose} maxWidth="sm" fullWidth>
      <DialogTitle>Party Configuration</DialogTitle>
      <DialogContent sx={{ p: 0, overflow: "hidden" }}>
        {loading ? (
          <Box sx={{ display: "flex", justifyContent: "center", py: 4 }}>
            <CircularProgress />
          </Box>
        ) : (
          <Box sx={{ position: "relative" }}>
            <Box
              sx={{
                position: "absolute",
                top: 0,
                left: 0,
                right: 0,
                height: 16,
                background:
                  "linear-gradient(to bottom, rgba(0,0,0,0.08), transparent)",
                pointerEvents: "none",
                opacity: canScrollUp ? 1 : 0,
                transition: "opacity 0.2s",
                zIndex: 1,
              }}
            />
            <Box
              ref={scrollRef}
              sx={{
                maxHeight: "60vh",
                overflowY: "auto",
                px: 3,
                py: 2,
                display: "flex",
                flexDirection: "column",
                gap: 2,
              }}
            >
              <TextField
                label="Member Party ID"
                value={memberPartyId}
                onChange={(e) => setMemberPartyId(e.target.value)}
                fullWidth
                size="small"
                disabled={saving}
              />

              <TextField
                label="User ID"
                value={userId}
                onChange={(e) => setUserId(e.target.value)}
                fullWidth
                size="small"
                disabled={saving}
              />

              <Divider />

              <Typography variant="subtitle2">Keycloak</Typography>

              <TextField
                label="URL"
                value={keycloakUrl}
                onChange={(e) => setKeycloakUrl(e.target.value)}
                fullWidth
                size="small"
                disabled={saving}
              />

              <TextField
                label="Realm"
                value={keycloakRealm}
                onChange={(e) => setKeycloakRealm(e.target.value)}
                fullWidth
                size="small"
                disabled={saving}
              />

              <Box>
                <Typography
                  variant="body2"
                  color="text.secondary"
                  sx={{ mb: 1 }}
                >
                  Credentials
                </Typography>
                <ToggleButtonGroup
                  value={authMethod}
                  exclusive
                  onChange={(_, val) => val && setAuthMethod(val)}
                  size="small"
                  disabled={saving}
                  fullWidth
                >
                  <ToggleButton value="client_credentials">
                    Client ID + Secret
                  </ToggleButton>
                  <ToggleButton value="password">
                    Username + Password
                  </ToggleButton>
                </ToggleButtonGroup>
              </Box>

              {authMethod === "client_credentials" ? (
                <>
                  <TextField
                    label="Client ID"
                    value={keycloakClientId}
                    onChange={(e) => setKeycloakClientId(e.target.value)}
                    fullWidth
                    size="small"
                    disabled={saving}
                  />
                  <TextField
                    label="Client Secret"
                    value={keycloakClientSecret}
                    onChange={(e) => setKeycloakClientSecret(e.target.value)}
                    fullWidth
                    size="small"
                    type="password"
                    disabled={saving}
                    placeholder="Enter new or leave empty to keep existing"
                  />
                </>
              ) : (
                <>
                  <TextField
                    label="Username"
                    value={keycloakUsername}
                    onChange={(e) => setKeycloakUsername(e.target.value)}
                    fullWidth
                    size="small"
                    disabled={saving}
                    placeholder="Enter new or leave empty to keep existing"
                  />
                  <TextField
                    label="Password"
                    value={keycloakPassword}
                    onChange={(e) => setKeycloakPassword(e.target.value)}
                    fullWidth
                    size="small"
                    type="password"
                    disabled={saving}
                    placeholder="Enter new or leave empty to keep existing"
                  />
                </>
              )}

              <Divider />

              <Typography variant="subtitle2">Packages</Typography>

              <TextField
                label="Vault Governance"
                value={packages.vault_governance ?? ""}
                onChange={(e) =>
                  setPackages({ ...packages, vault_governance: e.target.value })
                }
                fullWidth
                size="small"
                disabled={saving}
              />

              <TextField
                label="Vault"
                value={packages.vault ?? ""}
                onChange={(e) =>
                  setPackages({ ...packages, vault: e.target.value })
                }
                fullWidth
                size="small"
                disabled={saving}
              />

              <TextField
                label="Utility Registry"
                value={packages.utility_registry ?? ""}
                onChange={(e) =>
                  setPackages({
                    ...packages,
                    utility_registry: e.target.value,
                  })
                }
                fullWidth
                size="small"
                disabled={saving}
              />

              <TextField
                label="Utility Credential"
                value={packages.utility_credential ?? ""}
                onChange={(e) =>
                  setPackages({
                    ...packages,
                    utility_credential: e.target.value,
                  })
                }
                fullWidth
                size="small"
                disabled={saving}
              />

              {error && <Alert severity="error">{error}</Alert>}
              {success && (
                <Alert severity="success">Configuration saved.</Alert>
              )}
            </Box>
            <Box
              sx={{
                position: "absolute",
                bottom: 0,
                left: 0,
                right: 0,
                height: 16,
                background:
                  "linear-gradient(to top, rgba(0,0,0,0.08), transparent)",
                pointerEvents: "none",
                opacity: canScrollDown ? 1 : 0,
                transition: "opacity 0.2s",
                zIndex: 1,
              }}
            />
          </Box>
        )}
      </DialogContent>
      <DialogActions>
        <Button onClick={handleClose} disabled={saving}>
          {success ? "Close" : "Cancel"}
        </Button>
        <Button
          onClick={handleSave}
          variant="contained"
          disabled={saving || !memberPartyId || !userId}
        >
          {saving ? <CircularProgress size={20} /> : "Save"}
        </Button>
      </DialogActions>
    </Dialog>
  );
};
