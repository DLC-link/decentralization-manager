import { useState, useEffect, useCallback } from "react";
import {
  Accordion,
  AccordionSummary,
  AccordionDetails,
  Typography,
  Box,
  Button,
  Chip,
  CircularProgress,
  Alert,
} from "@mui/material";
import ExpandMoreIcon from "@mui/icons-material/ExpandMore";
import RefreshIcon from "@mui/icons-material/Refresh";
import CheckCircleIcon from "@mui/icons-material/CheckCircle";
import ErrorIcon from "@mui/icons-material/Error";
import HelpOutlineIcon from "@mui/icons-material/HelpOutline";
import ScienceIcon from "@mui/icons-material/Science";
import WarningIcon from "@mui/icons-material/Warning";
import { CopyableText } from "./CopyableText";
import { API_BASE } from "../constants";
import type { PartyAuthStatus, AuthStatusResponse, AuthTestResponse, RightsStatus } from "../types";

const accordionSx = {
  borderRadius: 2,
  mb: 2,
  "&:first-of-type": { borderRadius: 2 },
  "&:last-of-type": { borderRadius: 2 },
  overflow: "hidden",
};

export const AuthCheckAccordion = () => {
  const [authStatuses, setAuthStatuses] = useState<PartyAuthStatus[]>([]);
  const [loading, setLoading] = useState(true);
  const [testing, setTesting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const fetchAuthStatus = useCallback(async () => {
    try {
      setLoading(true);
      setError(null);
      const res = await fetch(`${API_BASE}/auth/status`);
      if (res.ok) {
        const data: AuthStatusResponse = await res.json();
        setAuthStatuses(data.parties);
      } else {
        setError("Failed to fetch auth status");
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : "Unknown error");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchAuthStatus();
  }, [fetchAuthStatus]);

  const handleTestAuth = async () => {
    try {
      setTesting(true);
      setError(null);
      const res = await fetch(`${API_BASE}/auth/test`, { method: "POST" });
      if (res.ok) {
        const data: AuthTestResponse = await res.json();
        // Update statuses based on test results
        setAuthStatuses((prev) =>
          prev.map((party) => {
            const result = data.results.find((r) => r.party_id === party.dec_party_id);
            if (result) {
              return {
                ...party,
                status: result.success
                  ? { status: "authenticated" as const }
                  : { status: "failed" as const, error: result.error || "Unknown error" },
              };
            }
            return party;
          })
        );
        // Refresh to get updated rights status
        fetchAuthStatus();
      } else {
        setError("Failed to test authentication");
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : "Unknown error");
    } finally {
      setTesting(false);
    }
  };

  const isRightsValid = (rights: RightsStatus | undefined): boolean => {
    if (!rights) return false;
    return (
      rights.member_party_act_as &&
      rights.member_party_read_as &&
      rights.dec_party_act_as &&
      rights.dec_party_read_as
    );
  };

  const getStatusIcon = (status: PartyAuthStatus["status"]) => {
    switch (status.status) {
      case "authenticated":
        return <CheckCircleIcon color="success" fontSize="small" />;
      case "mock":
        return <ScienceIcon color="warning" fontSize="small" />;
      case "failed":
        return <ErrorIcon color="error" fontSize="small" />;
      case "notconfigured":
        return <HelpOutlineIcon color="disabled" fontSize="small" />;
    }
  };

  const getStatusChip = (status: PartyAuthStatus["status"]) => {
    switch (status.status) {
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

  const hasParties = authStatuses.length > 0;

  return (
    <Accordion defaultExpanded sx={accordionSx}>
      <AccordionSummary
        expandIcon={<ExpandMoreIcon />}
        sx={{ borderRadius: "8px 8px 0 0" }}
      >
        <Box sx={{ display: "flex", alignItems: "center", gap: 1, width: "100%" }}>
          <Typography variant="h6">Authentication</Typography>
          {!loading && hasParties && (
            <Box sx={{ display: "flex", gap: 0.5, ml: 1 }}>
              {authStatuses.map((party) => (
                <Box key={party.dec_party_id}>{getStatusIcon(party.status)}</Box>
              ))}
            </Box>
          )}
        </Box>
      </AccordionSummary>
      <AccordionDetails sx={{ p: 3 }}>
        {loading ? (
          <Box sx={{ display: "flex", justifyContent: "center", p: 2 }}>
            <CircularProgress size={24} />
          </Box>
        ) : error ? (
          <Alert severity="error" sx={{ mb: 2 }}>
            {error}
          </Alert>
        ) : !hasParties ? (
          <Alert severity="info">
            No party credentials configured. Add a <code>[[parties]]</code> section to
            your node.toml configuration file.
          </Alert>
        ) : (
          <>
            <Box sx={{ mb: 2, display: "flex", gap: 1 }}>
              <Button
                variant="outlined"
                size="small"
                startIcon={testing ? <CircularProgress size={16} /> : <RefreshIcon />}
                onClick={handleTestAuth}
                disabled={testing}
              >
                {testing ? "Testing..." : "Test Authentication"}
              </Button>
            </Box>

            {authStatuses.map((party, index) => (
              <Box
                key={party.dec_party_id}
                sx={{
                  p: 2,
                  mb: index < authStatuses.length - 1 ? 2 : 0,
                  borderRadius: 1,
                  bgcolor: "background.default",
                  border: 1,
                  borderColor: "divider",
                }}
              >
                <Box
                  sx={{
                    display: "flex",
                    justifyContent: "space-between",
                    alignItems: "flex-start",
                    mb: 1,
                  }}
                >
                  <Box sx={{ display: "flex", alignItems: "center", gap: 1 }}>
                    <Typography variant="subtitle2">Dec Party</Typography>
                    <CopyableText
                      text={party.dec_party_id}
                      truncate={{ start: 16, end: 8 }}
                      variant="body2"
                    />
                  </Box>
                  {getStatusChip(party.status)}
                </Box>

                <Box sx={{ display: "flex", alignItems: "center", gap: 1, mb: 0.5 }}>
                  <Typography variant="body2" color="text.secondary">
                    <strong>Member Party:</strong>
                  </Typography>
                  <CopyableText
                    text={party.member_party_id}
                    truncate={{ start: 16, end: 8 }}
                    variant="body2"
                  />
                </Box>

                <Typography variant="body2" color="text.secondary" sx={{ mb: 0.5 }}>
                  <strong>User ID:</strong> {party.user_id}
                </Typography>
                {party.keycloak_url && (
                  <Typography variant="body2" color="text.secondary" sx={{ mb: 0.5 }}>
                    <strong>Keycloak URL:</strong> {party.keycloak_url}
                  </Typography>
                )}
                {party.keycloak_realm && (
                  <Typography variant="body2" color="text.secondary">
                    <strong>Realm:</strong> {party.keycloak_realm}
                  </Typography>
                )}

                {party.rights && (
                  <Box sx={{ mt: 1.5 }}>
                    <Typography variant="subtitle2" sx={{ mb: 0.5 }}>
                      User Rights
                      {isRightsValid(party.rights) ? (
                        <CheckCircleIcon color="success" fontSize="small" sx={{ ml: 1, verticalAlign: "middle" }} />
                      ) : (
                        <WarningIcon color="warning" fontSize="small" sx={{ ml: 1, verticalAlign: "middle" }} />
                      )}
                    </Typography>
                    <Box sx={{ display: "flex", flexWrap: "wrap", gap: 0.5 }}>
                      <Chip
                        label="Member actAs"
                        size="small"
                        color={party.rights.member_party_act_as ? "success" : "error"}
                        variant={party.rights.member_party_act_as ? "filled" : "outlined"}
                      />
                      <Chip
                        label="Member readAs"
                        size="small"
                        color={party.rights.member_party_read_as ? "success" : "error"}
                        variant={party.rights.member_party_read_as ? "filled" : "outlined"}
                      />
                      <Chip
                        label="DecParty actAs"
                        size="small"
                        color={party.rights.dec_party_act_as ? "success" : "error"}
                        variant={party.rights.dec_party_act_as ? "filled" : "outlined"}
                      />
                      <Chip
                        label="DecParty readAs"
                        size="small"
                        color={party.rights.dec_party_read_as ? "success" : "error"}
                        variant={party.rights.dec_party_read_as ? "filled" : "outlined"}
                      />
                    </Box>
                  </Box>
                )}

                {party.status.status === "mock" && (
                  <Alert severity="warning" sx={{ mt: 1 }}>
                    Running in test mode with mock authentication. Static JWT token is being used.
                  </Alert>
                )}
                {party.status.status === "failed" && (
                  <Alert severity="error" sx={{ mt: 1 }}>
                    {party.status.error}
                  </Alert>
                )}
                {party.rights && !isRightsValid(party.rights) && (
                  <Alert severity="warning" sx={{ mt: 1 }}>
                    Missing required user rights. Ensure the user has actAs and readAs permissions for both the member party and decentralized party.
                  </Alert>
                )}
              </Box>
            ))}
          </>
        )}
      </AccordionDetails>
    </Accordion>
  );
};
