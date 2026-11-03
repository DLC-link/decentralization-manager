import { useState } from "react";
import {
  Alert,
  Box,
  Button,
  CircularProgress,
  Dialog,
  DialogActions,
  DialogContent,
  DialogTitle,
  Divider,
  TextField,
  ToggleButton,
  ToggleButtonGroup,
  Typography,
} from "@mui/material";
import { API_BASE } from "../constants";
import { authenticatedFetch } from "../api";
import { fieldHelpAdornment } from "./FieldHelp";
import type {
  NodeIdentityRequest,
  NodeIdentityResponse,
  NodeLoginIdp,
  NodeLoginSummary,
} from "../types";

interface NodeIdentityDialogProps {
  open: boolean;
  onClose: () => void;
  onSaved: (response: NodeIdentityResponse) => void;
  /** The stored login, if any. Its fields prefill the form. */
  login?: NodeLoginSummary;
  /** Insecure mode: the node uses the unsafe token, so setup takes no login. */
  testMode?: boolean;
}

/** The login fields, prefilled from the stored login. Never a secret. */
const loginFields = (login?: NodeLoginSummary) => ({
  provider: login?.idp ?? ("keycloak" as NodeLoginIdp),
  keycloakUrl: login?.keycloak_url ?? "",
  keycloakRealm: login?.keycloak_realm ?? "",
  auth0Domain: login?.auth0_domain ?? "",
  auth0Audience: login?.auth0_audience ?? "",
  clientId: login?.client_id ?? "",
});

export const NodeIdentityDialog = ({
  open,
  onClose,
  onSaved,
  login,
  testMode = false,
}: NodeIdentityDialogProps) => {
  const [fields, setFields] = useState(() => loginFields(login));
  const [clientSecret, setClientSecret] = useState("");
  const [adminClientId, setAdminClientId] = useState("");
  const [adminClientSecret, setAdminClientSecret] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [wasOpen, setWasOpen] = useState(open);

  // Secrets never carry across opens. The other fields start from the stored
  // login, and the admin client id survives only a retry within one open.
  if (open !== wasOpen) {
    setWasOpen(open);
    setClientSecret("");
    setAdminClientSecret("");
    setError(null);
    if (open) setFields(loginFields(login));
    else setAdminClientId("");
  }

  const { provider, keycloakUrl, keycloakRealm, auth0Domain, auth0Audience, clientId } = fields;
  const setField = (key: keyof ReturnType<typeof loginFields>) => (value: string) =>
    setFields((current) => ({ ...current, [key]: value }));

  // The backend keeps the stored secret only for the same IdP at the same
  // URL or domain. Anything else needs the secret again.
  const storedSecretApplies =
    login?.idp === provider &&
    (provider === "keycloak"
      ? login.keycloak_url === keycloakUrl.trim()
      : login.auth0_domain === auth0Domain.trim());

  const loginComplete =
    clientId.trim().length > 0 &&
    (clientSecret.length > 0 || storedSecretApplies) &&
    (provider === "keycloak"
      ? keycloakUrl.trim().length > 0 && keycloakRealm.trim().length > 0
      : auth0Domain.trim().length > 0 && auth0Audience.trim().length > 0);

  const canSubmit =
    !saving &&
    (testMode ||
      (loginComplete && adminClientId.trim().length > 0 && adminClientSecret.length > 0));

  const requestBody = (): NodeIdentityRequest => {
    if (testMode) return {};
    const body: NodeIdentityRequest = {
      admin_client_id: adminClientId.trim(),
      admin_client_secret: adminClientSecret,
    };
    if (provider === "keycloak") {
      body.keycloak_url = keycloakUrl.trim();
      body.keycloak_realm = keycloakRealm.trim();
      body.keycloak_client_id = clientId.trim();
      if (clientSecret) body.keycloak_client_secret = clientSecret;
    } else {
      body.auth0_domain = auth0Domain.trim();
      body.auth0_audience = auth0Audience.trim();
      body.auth0_client_id = clientId.trim();
      if (clientSecret) body.auth0_client_secret = clientSecret;
    }
    return body;
  };

  const handleSubmit = async () => {
    setSaving(true);
    setError(null);
    const body = requestBody();
    try {
      const res = await authenticatedFetch(`${API_BASE}/node-identity`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      });
      setAdminClientSecret("");
      if (res.ok) {
        setClientSecret("");
        onSaved(await res.json());
        onClose();
      } else {
        const data = await res.json().catch(() => ({}));
        setError(data.error || "Failed to set up the node identity");
      }
    } catch (err) {
      setAdminClientSecret("");
      setError(err instanceof Error ? err.message : "Unknown error");
    } finally {
      setSaving(false);
    }
  };

  const handleClose = () => {
    if (!saving) onClose();
  };

  const secretPlaceholder = storedSecretApplies ? "Leave empty to keep the stored secret" : "Required";

  return (
    <Dialog open={open} onClose={handleClose} maxWidth="sm" fullWidth>
      <DialogTitle>Node Identity Login</DialogTitle>
      <DialogContent>
        <Box sx={{ display: "flex", flexDirection: "column", gap: 2, mt: 1 }}>
          {testMode ? (
            <Typography variant="body2" color="text.secondary">
              This node runs in insecure mode. It acts as its node party,{" "}
              <code>decman-node-…</code>, through the unsafe token, so setup takes
              no login and no admin credentials.
            </Typography>
          ) : (
            <>
              <Typography variant="body2" color="text.secondary">
                This node acts as its own party, <code>decman-node-…</code>, through
                one client-credentials login. The login&apos;s token names the
                Ledger API user. DecMan creates that user if it is missing and gives
                it <code>actAs</code> on the node party. The login is stored.
              </Typography>

              <ToggleButtonGroup
                value={provider}
                exclusive
                onChange={(_, value: NodeLoginIdp | null) => value && setField("provider")(value)}
                size="small"
                disabled={saving}
                fullWidth
              >
                <ToggleButton value="keycloak">Keycloak</ToggleButton>
                <ToggleButton value="auth0">Auth0</ToggleButton>
              </ToggleButtonGroup>

              {provider === "keycloak" ? (
                <>
                  <TextField
                    label="URL"
                    value={keycloakUrl}
                    onChange={(e) => setField("keycloakUrl")(e.target.value)}
                    size="small"
                    fullWidth
                    disabled={saving}
                    slotProps={{
                      input: {
                        endAdornment: fieldHelpAdornment(
                          "Base URL of the Keycloak server, e.g. https://keycloak.example.com.",
                          "Help for Keycloak URL",
                        ),
                      },
                    }}
                  />
                  <TextField
                    label="Realm"
                    value={keycloakRealm}
                    onChange={(e) => setField("keycloakRealm")(e.target.value)}
                    size="small"
                    fullWidth
                    disabled={saving}
                    slotProps={{
                      input: {
                        endAdornment: fieldHelpAdornment(
                          "The realm that holds the node's client. The admin client must be in the same realm.",
                          "Help for Keycloak Realm",
                        ),
                      },
                    }}
                  />
                </>
              ) : (
                <>
                  <TextField
                    label="Domain"
                    value={auth0Domain}
                    onChange={(e) => setField("auth0Domain")(e.target.value)}
                    size="small"
                    fullWidth
                    disabled={saving}
                    placeholder="tenant.us.auth0.com"
                    slotProps={{
                      input: {
                        endAdornment: fieldHelpAdornment(
                          "Your Auth0 tenant domain. The admin application must be in the same tenant.",
                          "Help for Auth0 Domain",
                        ),
                      },
                    }}
                  />
                  <TextField
                    label="Audience"
                    value={auth0Audience}
                    onChange={(e) => setField("auth0Audience")(e.target.value)}
                    size="small"
                    fullWidth
                    disabled={saving}
                    placeholder="https://your-api"
                    slotProps={{
                      input: {
                        endAdornment: fieldHelpAdornment(
                          "The identifier of the API that Canton's Ledger API accepts tokens for.",
                          "Help for Auth0 Audience",
                        ),
                      },
                    }}
                  />
                </>
              )}

              <TextField
                label="Node Client ID"
                value={clientId}
                onChange={(e) => setField("clientId")(e.target.value)}
                size="small"
                fullWidth
                disabled={saving}
                autoComplete="off"
                slotProps={{
                  input: {
                    endAdornment: fieldHelpAdornment(
                      "The client this node logs in with: a confidential Keycloak client with a service account, or an Auth0 machine-to-machine application. Use it for nothing else.",
                      "Help for Node Client ID",
                    ),
                  },
                }}
              />
              <TextField
                label="Node Client Secret"
                value={clientSecret}
                onChange={(e) => setClientSecret(e.target.value)}
                size="small"
                type="password"
                fullWidth
                disabled={saving}
                autoComplete="off"
                placeholder={secretPlaceholder}
                slotProps={{
                  input: {
                    endAdornment: fieldHelpAdornment(
                      "The secret of the node client. It is stored encrypted.",
                      "Help for Node Client Secret",
                    ),
                  },
                }}
              />

              <Divider />
              <Typography variant="body2" color="text.secondary">
                Admin credentials are used once and not stored. Paste a client
                whose identity holds <code>ParticipantAdmin</code> on this
                participant, at the same {provider === "keycloak" ? "realm" : "tenant"}.
              </Typography>
              <TextField
                label="Admin Client ID"
                value={adminClientId}
                onChange={(e) => setAdminClientId(e.target.value)}
                size="small"
                fullWidth
                disabled={saving}
                autoComplete="off"
              />
              <TextField
                label="Admin Client Secret"
                value={adminClientSecret}
                onChange={(e) => setAdminClientSecret(e.target.value)}
                size="small"
                type="password"
                fullWidth
                disabled={saving}
                autoComplete="off"
              />
            </>
          )}

          {error && (
            <Alert severity="error" onClose={() => setError(null)}>
              {error}
            </Alert>
          )}
        </Box>
      </DialogContent>
      <DialogActions>
        <Button onClick={handleClose} disabled={saving}>
          Cancel
        </Button>
        <Button
          onClick={handleSubmit}
          variant="contained"
          color="primary"
          disabled={!canSubmit}
        >
          {saving ? <CircularProgress size={20} color="inherit" /> : "Save"}
        </Button>
      </DialogActions>
    </Dialog>
  );
};
