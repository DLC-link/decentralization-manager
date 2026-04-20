import { useState } from "react";
import {
  Box,
  Typography,
  Chip,
  Button,
  CircularProgress,
  Alert,
  Collapse,
} from "@mui/material";
import ExpandMoreIcon from "@mui/icons-material/ExpandMore";
import ExpandLessIcon from "@mui/icons-material/ExpandLess";
import CheckCircleIcon from "@mui/icons-material/CheckCircle";
import ErrorIcon from "@mui/icons-material/Error";
import ScienceIcon from "@mui/icons-material/Science";
import WarningIcon from "@mui/icons-material/Warning";
import RefreshIcon from "@mui/icons-material/Refresh";
import { CopyableText } from "./CopyableText";
import { API_BASE } from "../constants";
import type { PartyAuthStatus, RightsStatus, AuthTestResponse } from "../types";

interface AuthSectionProps {
  partyId: string;
  authStatus?: PartyAuthStatus;
  onRefresh?: () => void;
}

export const AuthSection = ({ partyId, authStatus, onRefresh }: AuthSectionProps) => {
  const [expanded, setExpanded] = useState(false);
  const [testing, setTesting] = useState(false);
  const [testError, setTestError] = useState<string | null>(null);

  // Don't render if no auth configured for this party
  if (!authStatus) {
    return null;
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

  const getStatusIcon = () => {
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

  const getStatusChip = () => {
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

  const handleTestAuth = async () => {
    try {
      setTesting(true);
      setTestError(null);
      const res = await fetch(`${API_BASE}/auth/test`, { method: "POST" });
      if (res.ok) {
        const data: AuthTestResponse = await res.json();
        const result = data.results.find((r) => r.party_id === partyId);
        if (result && !result.success) {
          setTestError(result.error || "Authentication test failed");
        }
        // Refresh to get updated status
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
    <Box sx={{ mt: 2 }}>
      <Box
        sx={{
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          cursor: "pointer",
          py: 1,
        }}
        onClick={() => setExpanded(!expanded)}
      >
        <Box sx={{ display: "flex", alignItems: "center", gap: 1 }}>
          <Typography variant="subtitle2">Authentication</Typography>
          {getStatusIcon()}
        </Box>
        <Box sx={{ display: "flex", alignItems: "center", gap: 1 }}>
          {getStatusChip()}
          {expanded ? <ExpandLessIcon fontSize="small" /> : <ExpandMoreIcon fontSize="small" />}
        </Box>
      </Box>

      <Collapse in={expanded}>
        <Box
          sx={{
            p: 2,
            borderRadius: 1,
            bgcolor: "background.default",
            border: 1,
            borderColor: "divider",
          }}
        >
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
                  <CheckCircleIcon color="success" fontSize="small" sx={{ ml: 1, verticalAlign: "middle" }} />
                ) : (
                  <WarningIcon color="warning" fontSize="small" sx={{ ml: 1, verticalAlign: "middle" }} />
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

          <Box sx={{ mt: 2, mb: 2 }}>
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
      </Collapse>
    </Box>
  );
};
