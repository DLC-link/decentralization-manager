import { useState, useRef, useEffect, useCallback } from "react";
import {
  Box,
  Button,
  Chip,
  Collapse,
  Divider,
  IconButton,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableRow,
  Tabs,
  Tab,
  Tooltip,
  Typography,
} from "@mui/material";
import ArrowBackIcon from "@mui/icons-material/ArrowBack";
import PersonRemoveIcon from "@mui/icons-material/PersonRemove";
import UploadFileIcon from "@mui/icons-material/UploadFile";
import ExpandLessIcon from "@mui/icons-material/ExpandLess";
import ExpandMoreIcon from "@mui/icons-material/ExpandMore";
import SettingsIcon from "@mui/icons-material/Settings";
import { CopyableText } from "./CopyableText";
import { KickDialog } from "./KickDialog";
import { ContractsDialog } from "./ContractsDialog";
import { PartyConfigDialog } from "./PartyConfigDialog";
import { GovernanceSection } from "./GovernanceSection";
import { GovernanceAuditTrail } from "./GovernanceAuditTrail";
import { AuthSection } from "./AuthSection";
import { zebraRow } from "../styles";
import { ADMIN_ACCESS } from "../constants";
import type { DecentralizedParty, Network, PartyAuthStatus } from "../types";

interface PartyDetailProps {
  party: DecentralizedParty;
  onBack: () => void;
  onRefresh: () => void;
  selfParticipantId?: string;
  authStatus?: PartyAuthStatus;
  onAuthRefresh?: () => void;
  operatorParty?: string;
  network?: Network;
}

export const PartyDetail = ({
  party,
  onBack,
  onRefresh,
  selfParticipantId,
  authStatus,
  onAuthRefresh,
  operatorParty,
  network,
}: PartyDetailProps) => {
  const [kickDialogOpen, setKickDialogOpen] = useState(false);
  const [contractsDialogOpen, setContractsDialogOpen] = useState(false);
  const [configDialogOpen, setConfigDialogOpen] = useState(false);
  const [selectedParticipant, setSelectedParticipant] = useState("");
  const [contractsExpanded, setContractsExpanded] = useState(true);
  const [governanceTab, setGovernanceTab] = useState(0);
  const [canScrollUp, setCanScrollUp] = useState(false);
  const [canScrollDown, setCanScrollDown] = useState(false);
  const contractsScrollRef = useRef<HTMLDivElement>(null);

  const governanceContracts =
    party.contracts?.filter(
      (c) =>
        c.template_id.includes("VaultGovernanceRules") ||
        c.template_id.includes("VaultGovernance") ||
        c.template_id === "Governance.Rules:GovernanceRules",
    ) ?? [];
  const rulesContract = governanceContracts[0];
  const governanceType =
    rulesContract?.template_id === "Governance.Rules:GovernanceRules"
      ? ("core_self" as const)
      : ("vault" as const);

  const isOwner = Boolean(party.my_owner_key);

  const updateScrollShadows = useCallback(() => {
    const el = contractsScrollRef.current;
    if (el) {
      setCanScrollUp(el.scrollTop > 0);
      setCanScrollDown(el.scrollTop < el.scrollHeight - el.clientHeight - 1);
    }
  }, []);

  useEffect(() => {
    const el = contractsScrollRef.current;
    if (el) {
      updateScrollShadows();
      el.addEventListener("scroll", updateScrollShadows);
      return () => el.removeEventListener("scroll", updateScrollShadows);
    }
  }, [party.contracts, updateScrollShadows]);

  const handleKickClick = (participantUid: string) => {
    setSelectedParticipant(participantUid);
    setKickDialogOpen(true);
  };

  return (
    <Box>
      {/* Header */}
      <Box
        sx={{
          display: "flex",
          alignItems: "center",
          gap: 1,
          mb: 2,
        }}
      >
        <IconButton onClick={onBack}>
          <ArrowBackIcon />
        </IconButton>
        <CopyableText
          text={party.party_id}
          truncate={{ start: party.party_id.indexOf("::") + 18, end: 16 }}
          variant="h6"
        />
      </Box>

      <Box
        sx={{
          display: "flex",
          flexWrap: "wrap",
          gap: 1,
          mb: 3,
          alignItems: "center",
        }}
      >
        <Chip label={`Threshold: ${party.threshold}`} size="small" />
        <Chip label={`Owners: ${party.owners.length}`} size="small" />
        <Chip
          label={`Participants: ${party.participants.length}`}
          size="small"
        />
        {party.contracts && (
          <Chip
            label={`Contracts: ${party.contracts.length}`}
            size="small"
            color="primary"
          />
        )}
        <Tooltip title="Party configuration">
          <IconButton
            size="small"
            onClick={() => setConfigDialogOpen(true)}
          >
            <SettingsIcon fontSize="small" />
          </IconButton>
        </Tooltip>
        {isOwner && (
          <Button
            variant="outlined"
            size="small"
            startIcon={<UploadFileIcon />}
            onClick={() => setContractsDialogOpen(true)}
            disabled={!ADMIN_ACCESS}
          >
            {governanceType === "core_self"
              ? "Manage Plugins"
              : "Deploy Contracts"}
          </Button>
        )}
      </Box>

      {/* Owner Key */}
      {party.my_owner_key && (
        <Box sx={{ display: "flex", alignItems: "center", gap: 1, mb: 2 }}>
          <Typography variant="body2" color="text.secondary">
            <strong>My Owner Key:</strong>
          </Typography>
          <CopyableText
            text={party.my_owner_key}
            truncate={{ start: 16, end: 16 }}
            variant="body2"
          />
        </Box>
      )}

      <Divider sx={{ mb: 2 }} />

      {/* Participants */}
      <Typography variant="subtitle2" sx={{ mb: 1.5 }}>
        Participants
      </Typography>
      <Box sx={{ overflowX: "auto", mb: 2 }}>
        <Table size="small">
          <TableHead>
            <TableRow>
              <TableCell sx={{ py: 1 }}>Participant</TableCell>
              <TableCell sx={{ py: 1 }}>Permission</TableCell>
              <TableCell sx={{ py: 1 }} align="right">
                Actions
              </TableCell>
            </TableRow>
          </TableHead>
          <TableBody>
            {party.participants.map((p, idx) => (
              <TableRow key={p.participant_uid} sx={zebraRow(idx)}>
                <TableCell sx={{ py: 1 }}>
                  <CopyableText
                    text={p.participant_uid}
                    truncate={{ start: 32, end: 16 }}
                    variant="body2"
                  />
                </TableCell>
                <TableCell sx={{ py: 1 }}>
                  <Chip
                    label={p.permission}
                    size="small"
                    color={
                      p.permission === "submission" ? "success" : "default"
                    }
                  />
                </TableCell>
                <TableCell sx={{ py: 1 }} align="right">
                  <Tooltip
                    title={
                      p.participant_uid === selfParticipantId
                        ? "Cannot kick yourself"
                        : "Kick participant"
                    }
                  >
                    <span>
                      <IconButton
                        size="small"
                        color="error"
                        onClick={() => handleKickClick(p.participant_uid)}
                        disabled={
                          !ADMIN_ACCESS ||
                          p.participant_uid === selfParticipantId
                        }
                      >
                        <PersonRemoveIcon fontSize="small" />
                      </IconButton>
                    </span>
                  </Tooltip>
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </Box>

      {/* Contracts */}
      {party.contracts && party.contracts.length > 0 && (
        <>
          <Divider sx={{ mb: 2 }} />
          <Box
            sx={{
              display: "flex",
              alignItems: "center",
              cursor: "pointer",
              mb: 1,
            }}
            onClick={() => setContractsExpanded(!contractsExpanded)}
          >
            <IconButton size="small">
              {contractsExpanded ? <ExpandLessIcon /> : <ExpandMoreIcon />}
            </IconButton>
            <Typography variant="subtitle2">
              Contracts
              <Chip
                label={party.contracts.length}
                size="small"
                sx={{ ml: 1 }}
                color="primary"
              />
            </Typography>
          </Box>
          <Collapse in={contractsExpanded}>
            <Box sx={{ position: "relative", mb: 2 }}>
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
                ref={contractsScrollRef}
                sx={{
                  maxHeight: 180,
                  overflowY: "auto",
                  overflowX: "auto",
                }}
              >
                <Table size="small">
                  <TableHead>
                    <TableRow>
                      <TableCell sx={{ py: 1 }}>Template</TableCell>
                      <TableCell sx={{ py: 1 }}>Contract ID</TableCell>
                    </TableRow>
                  </TableHead>
                  <TableBody>
                    {party.contracts.map((c, idx) => (
                      <TableRow key={c.contract_id} sx={zebraRow(idx)}>
                        <TableCell sx={{ py: 1 }}>
                          {c.template_id}
                        </TableCell>
                        <TableCell sx={{ py: 1 }}>
                          <CopyableText
                            text={c.contract_id}
                            truncate={{ start: 16, end: 16 }}
                            variant="caption"
                          />
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
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
          </Collapse>
        </>
      )}

      {/* Auth & Governance */}
      {authStatus && (
        <>
          <Divider sx={{ mb: 2 }} />
          <AuthSection
            partyId={party.party_id}
            authStatus={authStatus}
            onRefresh={onAuthRefresh}
          />
          {authStatus.rights?.dec_party_act_as && (
            <>
              <Tabs
                value={governanceTab}
                onChange={(_e, v) => setGovernanceTab(v)}
                sx={{ mt: 2, borderBottom: 1, borderColor: "divider" }}
              >
                <Tab label="Governance" />
                <Tab label="Audit Trail" />
              </Tabs>
              {governanceTab === 0 && (
                <GovernanceSection
                  partyId={party.party_id}
                  rulesContractId={rulesContract?.contract_id}
                  governanceContractIds={governanceContracts.map(
                    (c) => c.contract_id,
                  )}
                  memberPartyId={authStatus.member_party_id}
                  defaultOperatorParty={operatorParty}
                  network={network}
                  governanceType={governanceType}
                />
              )}
              {governanceTab === 1 && (
                <GovernanceAuditTrail partyId={party.party_id} />
              )}
            </>
          )}
        </>
      )}

      {/* Dialogs */}
      <KickDialog
        open={kickDialogOpen}
        onClose={() => setKickDialogOpen(false)}
        onKickComplete={onRefresh}
        partyId={party.party_id}
        participantUid={selectedParticipant}
        participantOwnerKey={
          party.participants.find(
            (p) => p.participant_uid === selectedParticipant,
          )?.owner_key
        }
        currentThreshold={party.threshold}
        currentOwnerCount={party.owners.length}
      />

      <ContractsDialog
        open={contractsDialogOpen}
        onClose={() => setContractsDialogOpen(false)}
        onComplete={onRefresh}
        partyId={party.party_id}
        participantIds={party.participants.map((p) => p.participant_uid)}
        defaultOperatorParty={operatorParty}
        knownPackageIds={[
          ...new Set(party.contracts?.map((c) => c.package_id) ?? []),
        ]}
        deployedContracts={party.contracts ?? []}
      />

      <PartyConfigDialog
        open={configDialogOpen}
        onClose={() => setConfigDialogOpen(false)}
        onSave={() => {
          onRefresh();
          onAuthRefresh?.();
        }}
        partyId={party.party_id}
      />
    </Box>
  );
};
