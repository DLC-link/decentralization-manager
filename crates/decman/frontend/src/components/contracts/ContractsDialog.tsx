import { useState, useEffect, useMemo } from "react";
import { Dialog, DialogTitle, DialogContent, DialogActions, Button, Typography, CircularProgress, Alert, Box, TextField, IconButton, Divider } from "@mui/material";
import AddIcon from "@mui/icons-material/Add";
import DeleteIcon from "@mui/icons-material/Delete";
import ArrowBackIcon from "@mui/icons-material/ArrowBack";
import { API_BASE } from "../../constants";
import { authenticatedFetch } from "../../api";
import { useSnackbar } from "../../contexts";
import { useWorkflowStatus } from "../../hooks/useWorkflowStatus";
import { fieldHelpAdornment } from "../FieldHelp";
import type { ContractsStatusResponse, ContractsRequest, ContractDefinition, ContractInfo, PackageConfig, VettedPackageInfo } from "../../types";
import { createEmptyContract, commitPartyInput } from "./contractFields";
import { getContractsForType, withEmptyObserversAsNone, type ContractType } from "./contractPresets";
import { ContractEditor } from "./ContractEditor";
import { ContractTypeSelection } from "./ContractTypeSelection";

interface ContractsDialogProps {
  open: boolean;
  onClose: () => void;
  onComplete: () => void;
  partyId: string;
  participantIds: string[];
  defaultOperatorParty?: string;
  knownPackageIds?: string[];
  deployedContracts?: ContractInfo[];
}

export const ContractsDialog = ({
  open,
  onClose,
  onComplete,
  partyId,
  participantIds,
  defaultOperatorParty,
  knownPackageIds = [],
  deployedContracts = [],
}: ContractsDialogProps) => {
  const [vettedPackages, setVettedPackages] = useState<VettedPackageInfo[]>([]);
  const [vettedPackagesError, setVettedPackagesError] = useState<string | null>(
    null,
  );

  // Fetch fresh vetted packages when dialog opens
  useEffect(() => {
    if (open) {
      authenticatedFetch(`${API_BASE}/packages/vetted`)
        .then((res) => {
          if (!res.ok) throw new Error(`HTTP ${res.status}`);
          return res.json();
        })
        .then((data: VettedPackageInfo[]) => {
          setVettedPackages(data);
          setVettedPackagesError(null);
        })
        .catch((err: unknown) =>
          setVettedPackagesError(
            err instanceof Error ? err.message : "Unknown error",
          ),
        );
    }
  }, [open]);

  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<ContractsStatusResponse | null>(null);
  const [contractType, setContractType] = useState<ContractType>(null);
  const { showSnackbar } = useSnackbar();

  // Package config from API
  const [packages, setPackages] = useState<PackageConfig>({});

  // Form state
  const [operatorParty, setOperatorParty] = useState(
    defaultOperatorParty || "",
  );
  // Sync the autofetched operator party (from App.tsx) into local state once
  // it arrives — without this, the field stays empty whenever the fetch
  // completes after this component has already mounted with an empty default.
  useEffect(() => {
    if (defaultOperatorParty) setOperatorParty(defaultOperatorParty);
  }, [defaultOperatorParty]);
  const [participantParties, setParticipantParties] = useState<string[]>([]);
  const [contracts, setContracts] = useState<ContractDefinition[]>([]);

  // Governance Core deployment state
  const isGovernanceCoreDeployed = deployedContracts.some(
    (c) => c.template_id === "Governance.Rules:GovernanceRules",
  );
  const isDarUploaded = (patterns: string[]) =>
    vettedPackages.some((p) => {
      const name = p.package_name.toLowerCase();
      return patterns.some((pat) => name.includes(pat));
    });

  const isGovernanceCoreDarUploaded = isDarUploaded([
    "governance-core",
    "governance.rules",
  ]);
  const isTokenCustodyDarUploaded = isDarUploaded([
    "governance-token-custody",
    "tokencustody",
  ]);
  const isUtilityCredentialPluginDarUploaded = isDarUploaded([
    "governance-utility-credential",
  ]);

  // Combine package IDs from config + known contracts for dropdown
  const allPackageIds = useMemo(() => {
    const ids = new Set(knownPackageIds);
    if (packages.governance_core) ids.add(packages.governance_core);
    if (packages.utility_registry) ids.add(packages.utility_registry);
    if (packages.utility_credential) ids.add(packages.utility_credential);
    if (packages.utility_credential_app) ids.add(packages.utility_credential_app);
    return [...ids].sort();
  }, [knownPackageIds, packages]);

  // Fetch packages config when dialog opens
  useEffect(() => {
    if (open && partyId) {
      authenticatedFetch(
        `${API_BASE}/packages?party_id=${encodeURIComponent(partyId)}`,
      )
        .then((res) => res.json())
        .then((data: PackageConfig) => setPackages(data))
        .catch((e) => console.warn("Failed to fetch packages:", e));
    }
  }, [open, partyId]);

  // Reset state when dialog opens/closes
  useEffect(() => {
    if (!open) {
      setError(null);
      setStatus(null);
      setLoading(false);
      setContractType(null);
      setContracts([]);
      setOperatorParty(defaultOperatorParty || "");
      setParticipantParties([]);
      setPackages({});
    }
  }, [open, defaultOperatorParty]);

  // Initialize contracts when type is selected
  useEffect(() => {
    if (contractType) {
      setContracts(
        getContractsForType(contractType, participantIds.length, packages),
      );
    }
  }, [contractType, participantIds.length, packages]);

  // Pre-fill the Member Set for gov-core from each peer's configured member_party_id.
  useEffect(() => {
    if (contractType !== "governance-core") return;
    let cancelled = false;
    const fetchKnownMembers = async () => {
      try {
        const res = await authenticatedFetch(
          `${API_BASE}/governance/known-members?party_id=${encodeURIComponent(
            partyId,
          )}`,
        );
        if (!res.ok || cancelled) return;
        const data: {
          members: Array<{ participant_uid: string; member_party_id?: string }>;
        } = await res.json();
        const memberIds = data.members
          .map((m) => m.member_party_id)
          .filter((id): id is string => !!id && id.length > 0);
        if (cancelled || memberIds.length === 0) return;
        setContracts((prev) =>
          prev.map((c) => ({
            ...c,
            fields: c.fields.map((f) =>
              f.type === "party_set" ? { ...f, parties: memberIds } : f,
            ),
          })),
        );
      } catch {
        // If discovery fails, the operator can still type values manually.
      }
    };
    fetchKnownMembers();
    return () => {
      cancelled = true;
    };
  }, [contractType, partyId, packages]);

  useWorkflowStatus({
    path: `${API_BASE}/contracts/status`,
    cancelledMessage: "Contracts workflow cancelled",
    status,
    setStatus,
    setLoading,
    onComplete: onComplete,
    onClose,
  });

  const handleAddContract = () => {
    setContracts([...contracts, createEmptyContract()]);
  };

  const handleContractChange = (
    index: number,
    contract: ContractDefinition,
  ) => {
    const newContracts = [...contracts];
    newContracts[index] = contract;
    setContracts(newContracts);
  };

  const handleDeleteContract = (index: number) => {
    setContracts(contracts.filter((_, i) => i !== index));
  };

  const handleStart = async () => {
    setLoading(true);
    setError(null);

    // Validate required fields
    if (contractType !== "governance-core" && !operatorParty) {
      setError("Operator party ID is required");
      setLoading(false);
      return;
    }

    if (
      contractType !== "governance-core" &&
      participantParties.length !== participantIds.length
    ) {
      setError(
        `Please provide party IDs for all ${participantIds.length} participants`,
      );
      setLoading(false);
      return;
    }

    // For gov core the user doesn't fill the top section; derive participant_parties from the contract's Member Set field.
    const submittedParticipantParties =
      contractType === "governance-core"
        ? (contracts
            .flatMap((c) => c.fields)
            .find((f) => f.type === "party_set")?.parties ?? [])
        : participantParties;

    try {
      const request: ContractsRequest = {
        decentralized_party_id: partyId,
        participant_ids: participantIds,
        participant_parties: submittedParticipantParties,
        operator_party: operatorParty,
        contracts: contracts.map(withEmptyObserversAsNone),
      };

      const res = await authenticatedFetch(`${API_BASE}/contracts`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(request),
      });

      if (!res.ok) {
        const data = await res.json();
        throw new Error(data.error || "Failed to start contracts workflow");
      }

      showSnackbar("Contracts workflow started — follow progress in the feed");
      onClose();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Unknown error");
      setLoading(false);
    }
  };

  const [cancelling, setCancelling] = useState(false);
  const handleCancelWorkflow = async () => {
    setCancelling(true);
    try {
      const res = await authenticatedFetch(`${API_BASE}/contracts/cancel`, {
        method: "POST",
      });
      if (res.ok) {
        showSnackbar("Contracts workflow cancelled");
        onClose();
      } else {
        const data = await res.json().catch(() => ({}));
        setError(data.error || "Failed to cancel workflow");
      }
    } catch (err) {
      setError(
        err instanceof Error ? err.message : "Failed to cancel workflow",
      );
    } finally {
      setCancelling(false);
    }
  };

  const handleClose = () => {
    if (!loading) {
      onClose();
    }
  };

  const handleBack = () => {
    setContractType(null);
    setContracts([]);
  };

  const isInProgress = status?.status === "inprogress";
  const isCompleted = status?.status === "completed";
  const isFailed = status?.status === "failed";

  const getDialogTitle = () => {
    if (!contractType)
      return isGovernanceCoreDeployed ? "Plugin Manager" : "Deploy Contracts";
    if (contractType === "governance-core") return "Deploy Governance Core";
    return "Deploy CBTC Contracts";
  };

  return (
    <Dialog open={open} onClose={handleClose} maxWidth="md" fullWidth>
      <DialogTitle>
        <Box sx={{ display: "flex", alignItems: "center", gap: 1 }}>
          {contractType && !isInProgress && !isCompleted && (
            <IconButton size="small" onClick={handleBack} sx={{ mr: 1 }}>
              <ArrowBackIcon />
            </IconButton>
          )}
          {getDialogTitle()}
        </Box>
      </DialogTitle>
      <DialogContent>
        <Box sx={{ display: "flex", flexDirection: "column", gap: 2, mt: 1 }}>
          {error && (
            <Alert severity="error" onClose={() => setError(null)}>
              {error}
            </Alert>
          )}

          {isInProgress && (
            <Alert severity="info" icon={<CircularProgress size={20} />}>
              Contracts workflow in progress... This may take a few minutes.
            </Alert>
          )}

          {isCompleted && (
            <Alert severity="success">
              Contracts have been successfully deployed!
            </Alert>
          )}

          {isFailed && (
            <Alert severity="error">
              Contracts workflow failed: {status.error || "Unknown error"}
            </Alert>
          )}

          {!isInProgress && !isCompleted && !contractType && (
            <ContractTypeSelection
              onSelect={setContractType}
              isGovernanceCoreDeployed={isGovernanceCoreDeployed}
              isGovernanceCoreDarUploaded={isGovernanceCoreDarUploaded}
              isTokenCustodyDarUploaded={isTokenCustodyDarUploaded}
              isUtilityCredentialPluginDarUploaded={
                isUtilityCredentialPluginDarUploaded
              }
              vettedPackagesError={vettedPackagesError}
            />
          )}

          {!isInProgress && !isCompleted && contractType && (
            <>
              <Typography variant="body2" color="text.secondary">
                Configure and deploy contracts for the decentralized party. This
                will coordinate with other participants to sign and execute the
                submissions. Make sure DARs have been uploaded first.
              </Typography>

              {contractType !== "governance-core" && (
                <>
                  <Divider />
                  <Typography variant="subtitle1">
                    Party Configuration
                  </Typography>
                </>
              )}

              {contractType !== "governance-core" && (
                  <TextField
                    size="small"
                    label="Operator Party ID"
                    value={operatorParty}
                    onChange={(e) => setOperatorParty(e.target.value)}
                    fullWidth
                    required
                    error={!operatorParty}
                    helperText="Full party ID for the operator (e.g., operator::1220...)"
                    slotProps={{
                      input: {
                        endAdornment: fieldHelpAdornment(
                          "Full party ID of the operator party that administers these contracts (e.g. operator::1220...). It cannot be changed after the contracts are created.",
                          "Help for Operator Party ID",
                        ),
                      },
                    }}
                  />
                )}

              {contractType !== "governance-core" && (
                <>
                  <Typography variant="subtitle2" sx={{ mt: 1 }}>
                    Participant Party IDs ({participantParties.length}/
                    {participantIds.length})
                  </Typography>
                  <Typography
                    variant="body2"
                    color="text.secondary"
                    sx={{ mb: 1 }}
                  >
                    Enter the party ID for each participant. Must match the
                    order of participant IDs.
                  </Typography>
                  <Box
                    sx={{ display: "flex", flexDirection: "column", gap: 1 }}
                  >
                    <TextField
                      size="small"
                      placeholder="Paste party ID, press Enter or click away"
                      fullWidth
                      onKeyDown={(e) => {
                        if (e.key === "Enter") {
                          if (participantParties.length < participantIds.length) {
                            commitPartyInput(
                              e.currentTarget as HTMLInputElement,
                              participantParties,
                              setParticipantParties,
                            );
                          }
                          e.preventDefault();
                        }
                      }}
                      onBlur={(e) => {
                        if (participantParties.length < participantIds.length) {
                          commitPartyInput(
                            e.target,
                            participantParties,
                            setParticipantParties,
                          );
                        }
                      }}
                      disabled={
                        participantParties.length >= participantIds.length
                      }
                      slotProps={{
                        input: {
                          endAdornment: fieldHelpAdornment(
                            "Paste the full party ID for each participant and press Enter to add. Order must match the participant list above; the count must equal the total number of participants before you can deploy.",
                            "Help for Participant Party IDs",
                          ),
                        },
                      }}
                    />
                    {participantParties.length > 0 && (
                      <Box
                        sx={{
                          display: "flex",
                          flexDirection: "column",
                          gap: 0.5,
                        }}
                      >
                        {participantParties.map((party, idx) => (
                          <Box
                            key={idx}
                            sx={{
                              display: "flex",
                              alignItems: "center",
                              bgcolor: "action.hover",
                              borderRadius: 1,
                              px: 1,
                              py: 0.5,
                            }}
                          >
                            <Typography
                              variant="caption"
                              color="text.secondary"
                              sx={{ mr: 1, minWidth: 20 }}
                            >
                              {idx + 1}.
                            </Typography>
                            <Typography
                              variant="caption"
                              sx={{
                                flex: 1,
                                fontFamily: "var(--font-mono)",
                                overflow: "hidden",
                                textOverflow: "ellipsis",
                              }}
                            >
                              {party}
                            </Typography>
                            <IconButton
                              size="small"
                              onClick={() =>
                                setParticipantParties(
                                  participantParties.filter(
                                    (_, i) => i !== idx,
                                  ),
                                )
                              }
                              sx={{ p: 0.25 }}
                            >
                              <DeleteIcon sx={{ fontSize: 14 }} />
                            </IconButton>
                          </Box>
                        ))}
                      </Box>
                    )}
                  </Box>
                </>
              )}

              <Divider />
              <Box
                sx={{
                  display: "flex",
                  justifyContent: "space-between",
                  alignItems: "center",
                }}
              >
                <Typography variant="subtitle1">
                  Contract Definitions
                </Typography>
                {contractType !== "governance-core" && (
                  <Button
                    startIcon={<AddIcon />}
                    onClick={handleAddContract}
                    variant="outlined"
                    size="small"
                  >
                    Add Contract
                  </Button>
                )}
              </Box>

              {contracts.length === 0 ? (
                <Typography variant="body2" color="text.secondary">
                  {contractType === "governance-core"
                    ? "No contracts to deploy."
                    : 'No contracts defined. Click "Add Contract" to define contracts to deploy, or leave empty to skip contract creation.'}
                </Typography>
              ) : (
                contracts.map((contract, index) => (
                  <ContractEditor
                    key={index}
                    contract={contract}
                    onChange={(c) => handleContractChange(index, c)}
                    onDelete={() => handleDeleteContract(index)}
                    index={index}
                    participantCount={participantIds.length}
                    partyId={partyId}
                    knownPackageIds={allPackageIds}
                    lockStructure={contractType === "governance-core"}
                  />
                ))
              )}
            </>
          )}
        </Box>
      </DialogContent>
      <DialogActions>
        <Button onClick={handleClose} disabled={loading}>
          {isCompleted || isFailed || isInProgress ? "Close" : "Cancel"}
        </Button>
        {isInProgress && (
          <Button
            onClick={handleCancelWorkflow}
            variant="outlined"
            color="error"
            disabled={cancelling}
            startIcon={cancelling ? <CircularProgress size={16} /> : undefined}
          >
            {cancelling ? "Cancelling…" : "Cancel Workflow"}
          </Button>
        )}
        {contractType &&
        (!status?.status || status.status === "idle" || isFailed) ? (
          <Button
            onClick={handleStart}
            variant="contained"
            color="primary"
            disabled={loading}
          >
            {loading ? <CircularProgress size={20} /> : "Deploy Contracts"}
          </Button>
        ) : null}
      </DialogActions>
    </Dialog>
  );
};
