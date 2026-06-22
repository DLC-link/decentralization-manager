import {
  useState,
  useRef,
  useEffect,
  useCallback,
  type ReactNode,
} from "react";
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
  Tooltip,
  Typography,
} from "@mui/material";
import ArrowBackIcon from "@mui/icons-material/ArrowBack";
import EditIcon from "@mui/icons-material/Edit";
import PersonRemoveIcon from "@mui/icons-material/PersonRemove";
import UploadFileIcon from "@mui/icons-material/UploadFile";
import ExpandMoreIcon from "@mui/icons-material/ExpandMore";
import RefreshIcon from "@mui/icons-material/Refresh";
import { CopyableText } from "./CopyableText";
import { TextHelp } from "./FieldHelp";
import { KickDialog } from "./KickDialog";
import { ContractsDialog } from "./ContractsDialog";
import { PartyConfigDialog } from "./PartyConfigDialog";
import { GovernanceActionsDialog } from "./GovernanceActionsDialog";
import { GovernanceAuditTrail, CHAIN_LIMIT } from "./GovernanceAuditTrail";
import { HoldingsSection } from "./HoldingsSection";
import { AuthSection, getAuthStatusIcon } from "./AuthSection";
import { zebraRow } from "../styles";
import { ADMIN_ACCESS, API_BASE } from "../constants";
import { authenticatedFetch } from "../api";
import { formatMicroseconds } from "../governanceFormat";
import type {
  DecentralizedParty,
  GovernanceState,
  GovernanceStateResponse,
  Network,
  PartyAuthStatus,
} from "../types";

const StatCard = ({
  label,
  value,
  helpText,
}: {
  label: string;
  value: number | string;
  /// Optional plain-English explanation rendered as a tooltip on hover
  /// of the pill's label. No inline icon — keeps the chip compact.
  helpText?: string;
}) => {
  const labelNode = (
    <Typography
      variant="caption"
      color="text.secondary"
      sx={{ textTransform: "uppercase", letterSpacing: 0.6, fontWeight: 500 }}
    >
      {label}
    </Typography>
  );
  return (
    <Box
      sx={(theme) => ({
        display: "inline-flex",
        alignItems: "baseline",
        gap: 0.75,
        px: 1.5,
        py: 0.5,
        borderRadius: 999,
        backgroundColor:
          theme.palette.mode === "light"
            ? "rgba(0, 0, 0, 0.04)"
            : "rgba(255, 255, 255, 0.06)",
      })}
    >
      {helpText ? <TextHelp text={helpText}>{labelNode}</TextHelp> : labelNode}
      <Typography variant="body2" sx={{ fontWeight: 700 }}>
        {value}
      </Typography>
    </Box>
  );
};

interface CollapsibleSectionProps {
  title: string;
  expanded: boolean;
  onToggle: () => void;
  badge?: ReactNode;
  /// Optional plain-English explanation of the section's title word.
  /// Wraps the title in a `TextHelp` so hovering / focusing the title
  /// reveals a tooltip — no inline icon, since the title text itself is
  /// the trigger.
  helpText?: string;
  children: ReactNode;
}

const CollapsibleSection = ({
  title,
  expanded,
  onToggle,
  badge,
  helpText,
  children,
}: CollapsibleSectionProps) => (
  <>
    <Divider />
    <Box
      sx={(theme) => ({
        display: "flex",
        alignItems: "center",
        cursor: "pointer",
        py: 1,
        px: "var(--content-pad)",
        backgroundColor: expanded
          ? "transparent"
          : theme.palette.mode === "light"
            ? "rgba(0, 0, 0, 0.03)"
            : "rgba(255, 255, 255, 0.04)",
        transition: "background-color 0.2s ease",
      })}
      onClick={onToggle}
    >
      <ExpandMoreIcon
        fontSize="small"
        sx={{
          mr: 1,
          transform: expanded ? "rotate(180deg)" : "rotate(0deg)",
          transition: "transform 0.2s ease",
        }}
      />
      {helpText ? (
        <TextHelp text={helpText}>
          <Typography variant="subtitle2" component="span">
            {title}
          </Typography>
        </TextHelp>
      ) : (
        <Typography variant="subtitle2">{title}</Typography>
      )}
      {badge}
    </Box>
    <Collapse in={expanded}>{children}</Collapse>
  </>
);

interface PartyDetailProps {
  party: DecentralizedParty;
  onBack: () => void;
  onRefresh: () => void;
  /// Switch the app to the notifications tab. Called after the kick /
  /// contract-deployment workflows complete and after a governance action or
  /// proposal is submitted, so the user lands on the queue showing the
  /// resulting entry.
  onNavigateToNotifications: () => void;
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
  onNavigateToNotifications,
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
  const [participantsExpanded, setParticipantsExpanded] = useState(true);
  const [contractsExpanded, setContractsExpanded] = useState(false);
  const [holdingsExpanded, setHoldingsExpanded] = useState(false);
  const [authExpanded, setAuthExpanded] = useState(false);
  const [governanceExpanded, setGovernanceExpanded] = useState(false);
  const [holdingsCount, setHoldingsCount] = useState(0);
  const [holdingsLoading, setHoldingsLoading] = useState(false);
  const [holdingsRefreshNonce, setHoldingsRefreshNonce] = useState(0);
  const [governanceState, setGovernanceState] =
    useState<GovernanceState | null>(null);
  const [editGovContractId, setEditGovContractId] = useState<string | null>(
    null,
  );
  // Which half of the gov-actions dialog to show. Header "New Proposal" sets
  // "proposals"; per-contract pencil icon sets "actions".
  const [govDialogView, setGovDialogView] = useState<"actions" | "proposals">(
    "actions",
  );
  const [governanceRefreshNonce, setGovernanceRefreshNonce] = useState(0);
  const [auditTrailCount, setAuditTrailCount] = useState(0);
  const [auditTrailLoading, setAuditTrailLoading] = useState(false);
  const [canScrollUp, setCanScrollUp] = useState(false);
  const [canScrollDown, setCanScrollDown] = useState(false);
  const contractsScrollRef = useRef<HTMLDivElement>(null);

  const isGovRulesContract = (template_id: string) =>
    template_id.includes("VaultGovernanceRules") ||
    template_id.includes("VaultGovernance") ||
    template_id === "Governance.Rules:GovernanceRules";

  const governanceContracts =
    party.contracts?.filter((c) => isGovRulesContract(c.template_id)) ?? [];
  const rulesContract = governanceContracts[0];
  const governanceTypeFor = (template_id: string) =>
    template_id === "Governance.Rules:GovernanceRules"
      ? ("core_self" as const)
      : ("vault" as const);
  const governanceType = rulesContract
    ? governanceTypeFor(rulesContract.template_id)
    : ("vault" as const);

  const editingContract =
    editGovContractId != null
      ? party.contracts?.find((c) => c.contract_id === editGovContractId)
      : undefined;

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

  // Fetch governance state (threshold + action_confirmation_timeout) so the
  // contracts table can show these values on the row of the active rules
  // contract. Cancellation guards against a stale response landing after the
  // user has switched to a different party.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const res = await authenticatedFetch(
          `${API_BASE}/governance/state?party_id=${encodeURIComponent(party.party_id)}`,
        );
        if (!res.ok) return;
        const data: GovernanceStateResponse = await res.json();
        if (!cancelled) setGovernanceState(data.state);
      } catch {
        /* leave columns blank on failure */
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [party.party_id]);

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
          px: "var(--content-pad)",
        }}
      >
        <Tooltip title="Back to parties">
          <IconButton onClick={onBack}>
            <ArrowBackIcon />
          </IconButton>
        </Tooltip>
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
          px: "var(--content-pad)",
          alignItems: "center",
        }}
      >
        <StatCard
          label="Owners"
          value={party.owners.length}
          helpText="Owner keys that jointly control the party's decentralized namespace. Topology changes need a quorum of these — see Threshold."
        />
        <StatCard
          label="Threshold"
          value={party.threshold}
          helpText="Number of decentralized-namespace owners that must sign topology changes for this party (separate from the governance threshold below)."
        />
        {governanceState && (
          <>
            <StatCard
              label="Gov Threshold"
              value={governanceState.threshold}
              helpText="Number of governance-member confirmations required to execute a governance action on this party."
            />
            {governanceState.action_confirmation_timeout_microseconds !=
              null && (
              <StatCard
                label="Action Timeout"
                value={formatMicroseconds(
                  governanceState.action_confirmation_timeout_microseconds,
                )}
                helpText="How long an unexecuted action confirmation stays valid before it expires."
              />
            )}
          </>
        )}
        {isOwner && (
          <Button
            variant="outlined"
            size="small"
            startIcon={<UploadFileIcon />}
            onClick={() => {
              if (governanceType === "core_self" && rulesContract) {
                setGovDialogView("proposals");
                setEditGovContractId(rulesContract.contract_id);
              } else {
                setContractsDialogOpen(true);
              }
            }}
            disabled={!ADMIN_ACCESS}
          >
            {governanceType === "core_self"
              ? "New Proposal"
              : "Deploy Contracts"}
          </Button>
        )}
        {isOwner && rulesContract && (
          <Button
            variant="outlined"
            size="small"
            startIcon={<EditIcon />}
            onClick={() => {
              setGovDialogView("actions");
              setEditGovContractId(rulesContract.contract_id);
            }}
            disabled={!authStatus?.rights?.dec_party_act_as}
          >
            Governance Actions
          </Button>
        )}
      </Box>

      {/* Owner Key */}
      {party.my_owner_key && (
        <Box
          sx={{ display: "flex", alignItems: "center", gap: 1, mb: 2, px: "var(--content-pad)" }}
        >
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

      {/* Authentication */}
      <CollapsibleSection
        title="Authentication"
        expanded={authExpanded}
        onToggle={() => setAuthExpanded(!authExpanded)}
        helpText="Credentials this node uses to act on the party's behalf via the Canton ledger API."
        badge={
          <Box sx={{ display: "flex", alignItems: "center", ml: 1 }}>
            {getAuthStatusIcon(authStatus)}
          </Box>
        }
      >
        <Box sx={{ px: "var(--content-pad)" }}>
          <AuthSection
            partyId={party.party_id}
            authStatus={authStatus}
            onRefresh={onAuthRefresh}
            onConfigure={() => setConfigDialogOpen(true)}
          />
        </Box>
      </CollapsibleSection>

      {/* Participants */}
      <CollapsibleSection
        title="Participants"
        expanded={participantsExpanded}
        onToggle={() => setParticipantsExpanded(!participantsExpanded)}
        helpText="Canton participants hosting this party. One participant per row, with its permission level."
        badge={
          <Chip label={party.participants.length} size="small" sx={{ ml: 1 }} />
        }
      >
        <Box sx={{ overflowX: "auto" }}>
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
      </CollapsibleSection>

      {/* Contracts */}
      {party.contracts && party.contracts.length > 0 && (
        <CollapsibleSection
          title="Contracts"
          expanded={contractsExpanded}
          onToggle={() => setContractsExpanded(!contractsExpanded)}
          helpText="Daml contracts associated with the party — typically governance rules, vaults, registrar services, etc."
          badge={
            <Chip label={party.contracts.length} size="small" sx={{ ml: 1 }} />
          }
        >
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
                    <TableCell sx={{ py: 1 }}>Package</TableCell>
                    <TableCell sx={{ py: 1 }}>Version</TableCell>
                    <TableCell sx={{ py: 1 }}>Template</TableCell>
                    <TableCell sx={{ py: 1 }}>Created</TableCell>
                    <TableCell sx={{ py: 1 }}>Contract ID</TableCell>
                  </TableRow>
                </TableHead>
                <TableBody>
                  {party.contracts.map((c, idx) => (
                    <TableRow key={c.contract_id} sx={zebraRow(idx)}>
                      <TableCell sx={{ py: 1 }}>
                        {c.package_name || "—"}
                      </TableCell>
                      <TableCell sx={{ py: 1 }}>
                        {c.package_version || "—"}
                      </TableCell>
                      <TableCell sx={{ py: 1 }}>{c.template_id}</TableCell>
                      <TableCell sx={{ py: 1 }}>
                        {c.created_at
                          ? new Date(c.created_at).toLocaleString(undefined, {
                              year: "numeric",
                              month: "short",
                              day: "2-digit",
                              hour: "2-digit",
                              minute: "2-digit",
                            })
                          : "—"}
                      </TableCell>
                      <TableCell sx={{ py: 1 }}>
                        <CopyableText
                          text={c.contract_id}
                          truncate={{ start: 12, end: 12 }}
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
        </CollapsibleSection>
      )}

      {/* Holdings */}
      {authStatus?.rights?.dec_party_act_as && (
        <CollapsibleSection
          title="Holdings"
          expanded={holdingsExpanded}
          onToggle={() => setHoldingsExpanded(!holdingsExpanded)}
          helpText="Token-standard balances the party holds, aggregated by instrument."
          badge={
            <>
              {holdingsCount > 0 && (
                <Chip label={holdingsCount} size="small" sx={{ ml: 1 }} />
              )}
              <Tooltip title="Refresh holdings">
                <span>
                  <IconButton
                    size="small"
                    sx={{ ml: 0.5 }}
                    onClick={(e) => {
                      e.stopPropagation();
                      setHoldingsRefreshNonce((n) => n + 1);
                    }}
                    disabled={holdingsLoading}
                  >
                    <RefreshIcon fontSize="small" />
                  </IconButton>
                </span>
              </Tooltip>
            </>
          }
        >
          <HoldingsSection
            partyId={party.party_id}
            refreshNonce={holdingsRefreshNonce}
            onCountChange={setHoldingsCount}
            onLoadingChange={setHoldingsLoading}
          />
        </CollapsibleSection>
      )}

      {/* Audit Trail */}
      {authStatus?.rights?.dec_party_act_as && (
        <CollapsibleSection
          title="Audit Trail"
          expanded={governanceExpanded}
          onToggle={() => setGovernanceExpanded(!governanceExpanded)}
          helpText="On-chain history of governance actions and proposals for this party."
          badge={
            <>
              {auditTrailCount > 0 && (
                <Chip
                  label={`${auditTrailCount}${auditTrailCount === CHAIN_LIMIT ? "+" : ""}`}
                  size="small"
                  sx={{ ml: 1 }}
                />
              )}
              <Tooltip title="Refresh audit trail">
                <span>
                  <IconButton
                    size="small"
                    sx={{ ml: 0.5 }}
                    onClick={(e) => {
                      // Don't toggle the collapsible section.
                      e.stopPropagation();
                      setGovernanceRefreshNonce((n) => n + 1);
                    }}
                    disabled={auditTrailLoading}
                  >
                    <RefreshIcon fontSize="small" />
                  </IconButton>
                </span>
              </Tooltip>
            </>
          }
        >
          <GovernanceAuditTrail
            partyId={party.party_id}
            refreshNonce={governanceRefreshNonce}
            onCountChange={setAuditTrailCount}
            onLoadingChange={setAuditTrailLoading}
          />
        </CollapsibleSection>
      )}

      {/* Dialogs */}
      <KickDialog
        open={kickDialogOpen}
        onClose={() => setKickDialogOpen(false)}
        onKickComplete={() => {
          onRefresh();
          onNavigateToNotifications();
        }}
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
        onComplete={() => {
          onRefresh();
          onNavigateToNotifications();
        }}
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

      {editingContract && authStatus && (
        <GovernanceActionsDialog
          open={editGovContractId != null}
          onClose={() => setEditGovContractId(null)}
          partyId={party.party_id}
          rulesContractId={editingContract.contract_id}
          defaultOperatorParty={operatorParty}
          network={network}
          governanceType={governanceTypeFor(editingContract.template_id)}
          onAfterAction={() => {
            setGovernanceRefreshNonce((n) => n + 1);
            onNavigateToNotifications();
          }}
          view={govDialogView}
        />
      )}
    </Box>
  );
};
