import { useState } from "react";
import {
  Box,
  Typography,
  Chip,
  Button,
  CircularProgress,
  Alert,
  Tooltip,
} from "@mui/material";
import CheckCircleIcon from "@mui/icons-material/CheckCircle";
import ErrorIcon from "@mui/icons-material/Error";
import ScienceIcon from "@mui/icons-material/Science";
import WarningIcon from "@mui/icons-material/Warning";
import RefreshIcon from "@mui/icons-material/Refresh";
import { CopyableText } from "./CopyableText";
import { API_BASE } from "../constants";
import { authenticatedFetch } from "../api";
import type { PartyAuthStatus, RightsStatus, AuthTestResponse } from "../types";

interface AuthSectionProps {
  partyId: string;
  authStatus?: PartyAuthStatus;
  onRefresh?: () => void;
}

const isRightsValid = (rights: RightsStatus | undefined): boolean => {
  if (!rights) return false;
  return (
    rights.member_party_act_as &&
    rights.member_party_read_as &&
    rights.dec_party_act_as &&
    rights.dec_party_read_as
  );
};

const getAuthStatusIcon = (authStatus: PartyAuthStatus) => {
  switch (authStatus.status.status) {
    case "authenticated":
      return <CheckCircleIcon color="success" fontSize="small" />;
    case "mock":
      return <ScienceIcon color="warning" fontSize="small" />;
    case "failed":
      return <ErrorIcon color="error" fontSize="small" />;
    case "notconfigured":
      return null;
  }
};

const getAuthStatusChip = (authStatus: PartyAuthStatus) => {
  switch (authStatus.status.status) {
    case "authenticated":
      return <Chip label="Authenticated" color="success" size="small" />;
    case "mock":
      return <Chip label="Test Mode" color="warning" size="small" />;
    case "failed":
      return <Chip label="Failed" color="error" size="small" />;
    case "notconfigured":
      return <Chip label="Not Configured" color="default" size="small" />;
  }
};

export const AuthSection = ({ partyId, authStatus, onRefresh }: AuthSectionProps) => {
  const [testing, setTesting] = useState(false);
  const [testError, setTestError] = useState<string | null>(null);

  if (!authStatus) {
    return null;
  }

  const handleTestAuth = async () => {
    try {
      setTesting(true);
      setTestError(null);
      const res = await authenticatedFetch(`${API_BASE}/auth/test`, { method: "POST" });
      if (res.ok) {
        const data: AuthTestResponse = await res.json();
        const result = data.results.find((r) => r.party_id === partyId);
        if (result && !result.success) {
          setTestError(result.error || "Authentication test failed");
        }
        onRefresh?.();
      } else {
        setTestError("Failed to test authentication");
      }
    } catch (err) {
      setTestError(err instanceof Error ? err.message : "Unknown error");
    } finally {
      setTesting(false);
    }
  };

  return (
    <Box sx={{ py: 2 }}>
      <Box sx={{ display: "flex", alignItems: "center", gap: 1, mb: 1.5 }}>
        {getAuthStatusIcon(authStatus)}
        {getAuthStatusChip(authStatus)}
      </Box>

      <Box sx={{ display: "flex", alignItems: "center", gap: 1, mb: 1 }}>
        <Typography variant="body2" color="text.secondary">
          <strong>Member Party:</strong>
        </Typography>
        <CopyableText
          text={authStatus.member_party_id}
          truncate={{ start: 16, end: 8 }}
          variant="body2"
        />
      </Box>

      <Typography variant="body2" color="text.secondary" sx={{ mb: 0.5 }}>
        <strong>User ID:</strong> {authStatus.user_id}
      </Typography>
      {authStatus.keycloak_url && (
        <Typography variant="body2" color="text.secondary" sx={{ mb: 0.5 }}>
          <strong>Keycloak:</strong> {authStatus.keycloak_url}
        </Typography>
      )}

      {authStatus.rights && (
        <Box sx={{ mt: 1.5 }}>
          <Typography variant="subtitle2" sx={{ mb: 1 }}>
            User Rights
            {isRightsValid(authStatus.rights) ? (
              <Tooltip title="All required rights granted">
                <CheckCircleIcon color="success" fontSize="small" sx={{ ml: 1, verticalAlign: "middle" }} />
              </Tooltip>
            ) : (
              <Tooltip title="Missing required rights">
                <WarningIcon color="warning" fontSize="small" sx={{ ml: 1, verticalAlign: "middle" }} />
              </Tooltip>
            )}
          </Typography>
          <Box sx={{ display: "flex", flexDirection: "column", gap: 1 }}>
            <Box sx={{ display: "flex", alignItems: "center", gap: 1 }}>
              <Typography variant="body2" color="text.secondary" sx={{ minWidth: 100 }}>
                Dec Party:
              </Typography>
              <Chip
                label="actAs"
                size="small"
                variant="outlined"
                sx={{
                  borderColor: authStatus.rights.dec_party_act_as ? "success.main" : "grey.400",
                  color: authStatus.rights.dec_party_act_as ? "success.main" : "grey.500",
                }}
              />
              <Chip
                label="readAs"
                size="small"
                variant="outlined"
                sx={{
                  borderColor: authStatus.rights.dec_party_read_as ? "success.main" : "grey.400",
                  color: authStatus.rights.dec_party_read_as ? "success.main" : "grey.500",
                }}
              />
            </Box>
            <Box sx={{ display: "flex", alignItems: "center", gap: 1 }}>
              <Typography variant="body2" color="text.secondary" sx={{ minWidth: 100 }}>
                Member Party:
              </Typography>
              <Chip
                label="actAs"
                size="small"
                variant="outlined"
                sx={{
                  borderColor: authStatus.rights.member_party_act_as ? "success.main" : "grey.400",
                  color: authStatus.rights.member_party_act_as ? "success.main" : "grey.500",
                }}
              />
              <Chip
                label="readAs"
                size="small"
                variant="outlined"
                sx={{
                  borderColor: authStatus.rights.member_party_read_as ? "success.main" : "grey.400",
                  color: authStatus.rights.member_party_read_as ? "success.main" : "grey.500",
                }}
              />
            </Box>
          </Box>
        </Box>
      )}

      {authStatus.status.status === "mock" && (
        <Alert severity="warning" sx={{ mt: 1 }}>
          Running in test mode with mock authentication.
        </Alert>
      )}
      {authStatus.status.status === "failed" && (
        <Alert severity="error" sx={{ mt: 1 }}>
          {authStatus.status.error}
        </Alert>
      )}
      {authStatus.rights && !isRightsValid(authStatus.rights) && (
        <Alert severity="warning" sx={{ mt: 1 }}>
          Missing required user rights.
        </Alert>
      )}
      {testError && (
        <Alert severity="error" sx={{ mt: 1 }}>
          {testError}
        </Alert>
      )}

      <Box sx={{ mt: 2 }}>
        <Button
          variant="outlined"
          size="small"
          startIcon={testing ? <CircularProgress size={16} /> : <RefreshIcon />}
          onClick={handleTestAuth}
          disabled={testing}
        >
          {testing ? "Testing..." : "Test Auth"}
        </Button>
      </Box>
    </Box>
  );
};
