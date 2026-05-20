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
import { authenticatedFetch } from "../api";
import type {
  AuthConfig,
  PartyConfigResponse,
  PartyConfigRequest,
} from "../types";

type AuthMethod = "client_credentials" | "password";
type Provider = "keycloak" | "auth0";

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
  const [discovering, setDiscovering] = useState(false);
  const [discoverInfo, setDiscoverInfo] = useState<string | null>(null);

  const [provider, setProvider] = useState<Provider>("keycloak");

  const [memberPartyId, setMemberPartyId] = useState("");
  const [userId, setUserId] = useState("");

  // Keycloak fields
  const [keycloakUrl, setKeycloakUrl] = useState("");
  const [keycloakRealm, setKeycloakRealm] = useState("");
  const [keycloakClientId, setKeycloakClientId] = useState("");
  const [keycloakClientSecret, setKeycloakClientSecret] = useState("");
  const [keycloakUsername, setKeycloakUsername] = useState("");
  const [keycloakPassword, setKeycloakPassword] = useState("");
  const [authMethod, setAuthMethod] =
    useState<AuthMethod>("client_credentials");

  // Auth0 fields
  const [auth0Domain, setAuth0Domain] = useState("");
  const [auth0Audience, setAuth0Audience] = useState("");
  const [auth0ClientId, setAuth0ClientId] = useState("");
  const [auth0ClientSecret, setAuth0ClientSecret] = useState("");
  const [hasStoredAuth0Secret, setHasStoredAuth0Secret] = useState(false);

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
      setDiscoverInfo(null);
      fetchConfig();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, partyId]);

  const fetchConfig = async () => {
    setLoading(true);
    try {
      // Find the node-level provider so the dialog shows the matching
      // form. Falls back to keycloak if /auth-config is unreachable.
      let nodeProvider: Provider = "keycloak";
      try {
        const authRes = await fetch("/auth-config");
        if (authRes.ok) {
          const authCfg: AuthConfig = await authRes.json();
          if (authCfg.auth0_domain) nodeProvider = "auth0";
        }
      } catch {
        /* ignore */
      }

      const res = await authenticatedFetch(
        `${API_BASE}/party-config/${encodeURIComponent(partyId)}`,
      );
      if (res.ok) {
        const data: PartyConfigResponse = await res.json();
        setMemberPartyId(data.member_party_id ?? "");
        setUserId(data.user_id ?? "");
        setKeycloakUrl(data.keycloak_url);
        setKeycloakRealm(data.keycloak_realm);
        setKeycloakClientId(data.keycloak_client_id);
        setKeycloakClientSecret("");
        setKeycloakUsername("");
        setKeycloakPassword("");
        setAuthMethod(
          data.has_username || data.has_password
            ? "password"
            : "client_credentials",
        );

        setAuth0Domain(data.auth0_domain ?? "");
        setAuth0Audience(data.auth0_audience ?? "");
        setAuth0ClientId(data.auth0_client_id ?? "");
        setAuth0ClientSecret("");
        setHasStoredAuth0Secret(data.has_auth0_client_secret);

        // Prefer stored party-level provider, fall back to node-level.
        if (data.auth0_domain) setProvider("auth0");
        else setProvider(nodeProvider);
      } else {
        setProvider(nodeProvider);
      }
    } catch {
      // Config not found, fields stay empty
    } finally {
      setLoading(false);
    }
  };

  const canDiscover = (() => {
    if (provider === "auth0") {
      return (
        auth0Domain.trim().length > 0 &&
        auth0Audience.trim().length > 0 &&
        auth0ClientId.trim().length > 0 &&
        auth0ClientSecret.length > 0
      );
    }
    if (
      !keycloakUrl.trim() ||
      !keycloakRealm.trim() ||
      !keycloakClientId.trim()
    ) {
      return false;
    }
    if (authMethod === "client_credentials") {
      return keycloakClientSecret.length > 0;
    }
    return keycloakUsername.length > 0 && keycloakPassword.length > 0;
  })();

  const handleDiscover = async () => {
    setDiscovering(true);
    setError(null);
    setDiscoverInfo(null);
    try {
      const body: Record<string, string> = {};
      if (provider === "auth0") {
        body.auth0_domain = auth0Domain.trim();
        body.auth0_audience = auth0Audience.trim();
        body.auth0_client_id = auth0ClientId.trim();
        body.auth0_client_secret = auth0ClientSecret;
      } else {
        body.keycloak_url = keycloakUrl.trim();
        body.keycloak_realm = keycloakRealm.trim();
        body.keycloak_client_id = keycloakClientId.trim();
        if (authMethod === "client_credentials") {
          body.keycloak_client_secret = keycloakClientSecret;
        } else {
          body.keycloak_username = keycloakUsername;
          body.keycloak_password = keycloakPassword;
        }
      }
      const res = await authenticatedFetch(
        `${API_BASE}/party-config/discover-member-party`,
        {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(body),
        },
      );
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        setError(data.error || "Discovery failed");
        return;
      }
      const data: {
        user_id: string;
        primary_party?: string;
        description?: string;
      } = await res.json();
      if (data.user_id) setUserId(data.user_id);
      if (data.primary_party) {
        setMemberPartyId(data.primary_party);
        setDiscoverInfo(
          data.description
            ? `Found ${data.description}`
            : "Member party discovered.",
        );
      } else {
        setDiscoverInfo(
          "Authenticated, but Canton has no primary party for this user. Enter the member party manually.",
        );
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : "Discovery failed");
    } finally {
      setDiscovering(false);
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
    };

    if (provider === "auth0") {
      body.auth0_domain = auth0Domain.trim();
      body.auth0_audience = auth0Audience.trim();
      body.auth0_client_id = auth0ClientId.trim();
      // Empty input = keep existing when one is stored, otherwise required.
      if (auth0ClientSecret.length > 0) {
        body.auth0_client_secret = auth0ClientSecret;
      }
    } else {
      body.keycloak_url = keycloakUrl;
      body.keycloak_realm = keycloakRealm;
      body.keycloak_client_id =
        authMethod === "client_credentials" ? keycloakClientId : "";

      if (authMethod === "client_credentials") {
        body.keycloak_client_secret = keycloakClientSecret || undefined;
        body.keycloak_username = "";
        body.keycloak_password = "";
      } else {
        body.keycloak_client_secret = "";
        body.keycloak_username = keycloakUsername || undefined;
        body.keycloak_password = keycloakPassword || undefined;
      }
    }

    try {
      const res = await authenticatedFetch(`${API_BASE}/party-config`, {
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

  const canSave =
    !!memberPartyId &&
    !!userId &&
    (provider === "auth0"
      ? auth0Domain.trim().length > 0 &&
        auth0Audience.trim().length > 0 &&
        auth0ClientId.trim().length > 0 &&
        (auth0ClientSecret.length > 0 || hasStoredAuth0Secret)
      : true);

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

              {provider === "auth0" ? (
                <>
                  <Typography variant="subtitle2">Auth0</Typography>

                  <TextField
                    label="Domain"
                    value={auth0Domain}
                    onChange={(e) => setAuth0Domain(e.target.value)}
                    fullWidth
                    size="small"
                    disabled={saving}
                    placeholder="tenant.us.auth0.com"
                  />

                  <TextField
                    label="Audience"
                    value={auth0Audience}
                    onChange={(e) => setAuth0Audience(e.target.value)}
                    fullWidth
                    size="small"
                    disabled={saving}
                    placeholder="https://your-api"
                  />

                  <TextField
                    label="Client ID"
                    value={auth0ClientId}
                    onChange={(e) => setAuth0ClientId(e.target.value)}
                    fullWidth
                    size="small"
                    disabled={saving}
                  />

                  <TextField
                    label="Client Secret"
                    value={auth0ClientSecret}
                    onChange={(e) => setAuth0ClientSecret(e.target.value)}
                    fullWidth
                    size="small"
                    type="password"
                    disabled={saving}
                    placeholder={
                      hasStoredAuth0Secret
                        ? "Leave empty to keep existing"
                        : "Required"
                    }
                  />
                </>
              ) : (
                <>
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
                        onChange={(e) =>
                          setKeycloakClientSecret(e.target.value)
                        }
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
                </>
              )}

              <Box>
                <Button
                  size="small"
                  variant="outlined"
                  onClick={handleDiscover}
                  disabled={!canDiscover || discovering || saving}
                  startIcon={
                    discovering ? <CircularProgress size={16} /> : undefined
                  }
                >
                  {discovering ? "Discovering…" : "Discover Member Party"}
                </Button>
              </Box>
              {discoverInfo && <Alert severity="success">{discoverInfo}</Alert>}

              {error && (
                <Alert severity="error" onClose={() => setError(null)}>
                  {error}
                </Alert>
              )}
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
          disabled={saving || !canSave}
        >
          {saving ? <CircularProgress size={20} /> : "Save"}
        </Button>
      </DialogActions>
    </Dialog>
  );
};
