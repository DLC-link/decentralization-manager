import { useState, useRef, useEffect, useCallback, type ReactNode } from "react";
import { Box, Button, Chip, IconButton, Tooltip, Typography } from "@mui/material";
import ArrowBackIcon from "@mui/icons-material/ArrowBack";
import EditIcon from "@mui/icons-material/Edit";
import TuneIcon from "@mui/icons-material/Tune";
import PersonAddIcon from "@mui/icons-material/PersonAdd";
import UploadFileIcon from "@mui/icons-material/UploadFile";
import RefreshIcon from "@mui/icons-material/Refresh";
import CompareArrowsIcon from "@mui/icons-material/CompareArrows";
import { CopyableText } from "./CopyableText";
import { StatCard } from "./StatCard";
import { usePagination } from "../usePagination";
import { AddPartyDialog } from "./AddPartyDialog";
import { ChangeThresholdDialog } from "./ChangeThresholdDialog";
import { KickDialog } from "./KickDialog";
import { ContractsDialog } from "./contracts/ContractsDialog";
import { PartyConfigDialog } from "./PartyConfigDialog";
import { GovernanceActionsDialog } from "./GovernanceActionsDialog";
import { GovernanceAuditTrail } from "./GovernanceAuditTrail";
import { GovernanceRules } from "./GovernanceRules";
import { HoldingsSection } from "./HoldingsSection";
import { RewardStatus } from "./RewardStatus";
import { AuthSection, getAuthStatusIcon } from "./AuthSection";
import { ADMIN_ACCESS, API_BASE } from "../constants";
import { authenticatedFetch } from "../api";
import { formatMicroseconds } from "../governanceFormat";
import { getGovernanceMembership } from "../governanceMembership";
import { usePeers, useSharedPartyCounts } from "../usePeerLinks";
import type { DecentralizedParty, GovernanceState, GovernanceStateResponse, Network, NonHostingOwner, PartyAuthStatus, RewardStatusResponse } from "../types";
import { CollapsibleSection } from "./CollapsibleSection";
import { PartyParticipantsTable } from "./PartyParticipantsTable";
import { PartyContractsTable } from "./PartyContractsTable";

const GOVERNANCE_STATE_POLL_MS = 10_000;
// Slower than the governance poll: a delegation changes only by a vote, and
// under mock auth each read scans the party's whole ACS.
const REWARD_STATUS_POLL_MS = 30_000;

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
  /// Open the packages tab, comparing this party's hosting participants.
  onCheckDars?: () => void;
  /// Open the configuration tab at a peer's row.
  onOpenPeerConfig?: (participantId: string) => void;
  /// Open the party list filtered to the parties a participant also hosts.
  onShowSharedParties?: (participantId: string) => void;
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
  onCheckDars,
  onOpenPeerConfig,
  onShowSharedParties,
}: PartyDetailProps) => {
  const peers = usePeers();
  const sharedCounts = useSharedPartyCounts();
  const [kickDialogOpen, setKickDialogOpen] = useState(false);
  const [addDialogOpen, setAddDialogOpen] = useState(false);
  const [changeThresholdDialogOpen, setChangeThresholdDialogOpen] =
    useState(false);
  const [contractsDialogOpen, setContractsDialogOpen] = useState(false);
  const [configDialogOpen, setConfigDialogOpen] = useState(false);
  const [selectedParticipant, setSelectedParticipant] = useState("");
  // Set when the kick targets an owner that does not host the party.
  const [selectedOwner, setSelectedOwner] = useState<NonHostingOwner | null>(
    null,
  );
  const [participantsExpanded, setParticipantsExpanded] = useState(true);
  const [contractsExpanded, setContractsExpanded] = useState(false);
  const [holdingsExpanded, setHoldingsExpanded] = useState(false);
  const [authExpanded, setAuthExpanded] = useState(false);
  const [governanceExpanded, setGovernanceExpanded] = useState(false);
  const [rulesExpanded, setRulesExpanded] = useState(true);
  const [rewardsExpanded, setRewardsExpanded] = useState(true);
  // Keyed by party, like the governance state, so a late response for the
  // previous party never shows.
  const [loadedRewards, setLoadedRewards] = useState<{
    partyId: string;
    status: RewardStatusResponse | null;
    error: string | null;
  } | null>(null);
  const rewards =
    loadedRewards?.partyId === party.party_id ? loadedRewards : null;
  const [holdingsCount, setHoldingsCount] = useState(0);
  const [holdingsLoading, setHoldingsLoading] = useState(false);
  const [holdingsRefreshNonce, setHoldingsRefreshNonce] = useState(0);
  const [loadedGovernance, setLoadedGovernance] = useState<{
    partyId: string;
    state: GovernanceState | null;
  } | null>(null);
  const governanceState =
    loadedGovernance?.partyId === party.party_id
      ? loadedGovernance.state
      : null;
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
  // Whether older audit entries exist beyond the loaded page, straight from the
  // server's cursor. The page size is not a reliable stand-in: it can overshoot
  // to keep a transaction whole.
  const [auditTrailHasMore, setAuditTrailHasMore] = useState(false);
  const [auditTrailLoading, setAuditTrailLoading] = useState(false);
  const [canScrollUp, setCanScrollUp] = useState(false);
  const [canScrollDown, setCanScrollDown] = useState(false);
  const contractsPaging = usePagination(party.contracts ?? []);
  const contractsScrollRef = useRef<HTMLDivElement>(null);

  const isGovRulesContract = (template_id: string) =>
    template_id === "Governance.Rules:GovernanceRules";

  const governanceContracts =
    party.contracts?.filter((c) => isGovRulesContract(c.template_id)) ?? [];
  const rulesContract = governanceContracts[0];

  const editingContract =
    editGovContractId != null
      ? party.contracts?.find((c) => c.contract_id === editGovContractId)
      : undefined;

  const isOwner = Boolean(party.my_owner_key);

  // `authStatus.rights` describes the party credential this node is configured
  // with, not the person using the browser. `canAct` gates the one control that
  // submits a command; `canRead` gates the sections that only read, so a node
  // configured read-only still shows them.
  const canAct = Boolean(authStatus?.rights?.dec_party_act_as);
  const canRead = canAct || Boolean(authStatus?.rights?.dec_party_read_as);
  const membership = getGovernanceMembership(
    authStatus?.member_party_id,
    governanceState,
  );

  // Stable identity: the audit trail reports through this from an effect, so a
  // fresh callback each render would re-run it on every parent render.
  const onAuditTrailCountChange = useCallback(
    (count: number, hasMore: boolean) => {
      setAuditTrailCount(count);
      setAuditTrailHasMore(hasMore);
    },
    [],
  );

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

  // Poll governance state for the rules contract row in the contracts table and
  // the membership chip in the auth section. The party id on each result keeps
  // a late response for a previous party from showing.
  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      try {
        const res = await authenticatedFetch(
          `${API_BASE}/governance/state?party_id=${encodeURIComponent(party.party_id)}`,
        );
        if (!res.ok) return;
        const data: GovernanceStateResponse = await res.json();
        if (!cancelled)
          setLoadedGovernance({
            partyId: party.party_id,
            state: data.state ?? null,
          });
      } catch {
        /* keep the last loaded state */
      }
    };
    void load();
    const interval = setInterval(() => void load(), GOVERNANCE_STATE_POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, [party.party_id]);

  // The reward status reads the ledger as the party, so it needs the same
  // rights as the other read-only sections. A failed read stays an error: it
  // must never look like a missing DAR or a missing delegation. The poll picks
  // up a new delegation, and clears an error once a read succeeds.
  useEffect(() => {
    if (!canRead) return;
    let cancelled = false;
    const load = async () => {
      let status: RewardStatusResponse | null = null;
      let error: string | null = null;
      try {
        const res = await authenticatedFetch(
          `${API_BASE}/reward-status?party_id=${encodeURIComponent(party.party_id)}`,
        );
        if (res.ok) {
          status = await res.json();
        } else {
          const body: { error?: string } = await res.json().catch(() => ({}));
          error = body.error || `the request failed with status ${res.status}`;
        }
      } catch (e) {
        error = e instanceof Error ? e.message : String(e);
      }
      if (!cancelled)
        setLoadedRewards({ partyId: party.party_id, status, error });
    };
    void load();
    const interval = setInterval(() => void load(), REWARD_STATUS_POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, [party.party_id, canRead]);

  const handleKickClick = (participantUid: string) => {
    setSelectedOwner(null);
    setSelectedParticipant(participantUid);
    setKickDialogOpen(true);
  };

  const handleKickOwnerClick = (owner: NonHostingOwner) => {
    setSelectedOwner(owner);
    setKickDialogOpen(true);
  };

  // The server leaves the field out when every owner hosts the party.
  const nonHostingOwners = party.non_hosting_owners ?? [];

  const prefix = party.party_id.split("::")[0];
  const actions = [
    isOwner && {
      key: "propose",
      icon: <UploadFileIcon fontSize="small" />,
      label: "New Proposal",
      disabled: !ADMIN_ACCESS,
      onClick: () => {
        if (rulesContract) {
          setGovDialogView("proposals");
          setEditGovContractId(rulesContract.contract_id);
        } else {
          setContractsDialogOpen(true);
        }
      },
    },
    isOwner && {
      key: "threshold",
      icon: <TuneIcon fontSize="small" />,
      label: "Change Threshold",
      disabled: !ADMIN_ACCESS,
      onClick: () => setChangeThresholdDialogOpen(true),
    },
    isOwner &&
      rulesContract && {
        key: "gov",
        icon: <EditIcon fontSize="small" />,
        label: "Governance Actions",
        disabled: !canAct,
        onClick: () => {
          setGovDialogView("actions");
          setEditGovContractId(rulesContract.contract_id);
        },
      },
    onCheckDars && {
      key: "check-dars",
      icon: <CompareArrowsIcon fontSize="small" />,
      label: "Check DARs",
      disabled: false,
      onClick: onCheckDars,
    },
  ].filter(Boolean) as {
    key: string;
    icon: ReactNode;
    label: string;
    disabled: boolean;
    onClick: () => void;
  }[];

  return (
    <Box
      sx={{
        px: "var(--content-pad)",
        pt: 2,
        pb: 4,
        display: "flex",
        flexDirection: "column",
        gap: 2,
      }}
    >
      {/* Identity: the party's own name leads, its full id sits under it, and
        * the actions gather on the right instead of competing with the figures. */}
      <Box
        sx={{
          display: "flex",
          alignItems: "flex-start",
          gap: 1.5,
          flexWrap: "wrap",
        }}
      >
        <Tooltip title="Back to parties">
          <IconButton onClick={onBack} sx={{ mt: 0.25, ml: "-8px" }}>
            <ArrowBackIcon />
          </IconButton>
        </Tooltip>
        <Box sx={{ flex: 1, minWidth: 240 }}>
          <Typography
            sx={{ fontSize: 24, fontWeight: 600, lineHeight: 1.2 }}
            data-testid="party-detail-title"
          >
            {prefix}
          </Typography>
          <Box sx={{ display: "flex", alignItems: "center", gap: 2, flexWrap: "wrap", mt: 0.25 }}>
            <CopyableText
              text={party.party_id}
              truncate={{ start: prefix.length + 12, end: 12 }}
              variant="body2"
            />
            {party.my_owner_key && (
              <Box sx={{ display: "flex", alignItems: "center", gap: 0.75 }}>
                <Typography variant="caption" color="text.secondary">
                  my owner key
                </Typography>
                <CopyableText
                  text={party.my_owner_key}
                  truncate={{ start: 10, end: 8 }}
                  variant="body2"
                />
              </Box>
            )}
          </Box>
        </Box>
        {actions.length > 0 && (
          <Box sx={{ display: "flex", gap: 1, flexWrap: "wrap" }}>
            {actions.map((a) => (
              <Button
                key={a.key}
                variant="outlined"
                size="small"
                color="inherit"
                startIcon={a.icon}
                onClick={a.onClick}
                disabled={a.disabled}
                sx={{ color: "text.secondary", borderColor: "divider" }}
              >
                {a.label}
              </Button>
            ))}
          </Box>
        )}
      </Box>

      {/* The figures, at a size worth reading. */}
      <Box sx={{ display: "flex", gap: 1.5, flexWrap: "wrap" }}>
        <StatCard
          label="Owners"
          value={party.owners.length}
          helpText="Owner keys that jointly control the party's decentralized namespace. Topology changes need a quorum of these — see Threshold."
        />
        <StatCard
          label="Threshold"
          value={party.threshold}
          unit={`of ${party.owners.length}`}
          helpText="Number of decentralized-namespace owners that must sign topology changes for this party (separate from the governance threshold)."
          testId="party-threshold"
        />
        {party.confirmation_threshold != null && (
          <StatCard
            label="Confirmation Threshold"
            value={party.confirmation_threshold}
            unit={`of ${party.participants.length}`}
            helpText="Number of hosting participants that must confirm each transaction for this party."
            testId="party-confirmation-threshold"
          />
        )}
        {governanceState && (
          <StatCard
            label="Gov Threshold"
            value={governanceState.threshold}
            helpText="Number of governance-member confirmations required to execute a governance action on this party."
          />
        )}
        {governanceState?.action_confirmation_timeout_microseconds != null && (
          <StatCard
            label="Action Timeout"
            value={formatMicroseconds(
              governanceState.action_confirmation_timeout_microseconds,
            )}
            helpText="How long an unexecuted action confirmation stays valid before it expires."
          />
        )}
      </Box>

      {/* Authentication */}
      <CollapsibleSection
        title="Authentication"
        expanded={authExpanded}
        onToggle={() => setAuthExpanded(!authExpanded)}
        helpText="Credentials this node uses to act on the party's behalf via the Canton ledger API."
        badge={
          <Box sx={{ display: "flex", alignItems: "center", ml: 1 }}>
            {getAuthStatusIcon(authStatus, membership)}
          </Box>
        }
      >
        <Box sx={{ px: "var(--content-pad)" }}>
          <AuthSection
            partyId={party.party_id}
            authStatus={authStatus}
            membership={membership}
            onRefresh={onAuthRefresh}
            onConfigure={() => setConfigDialogOpen(true)}
          />
        </Box>
      </CollapsibleSection>

      {/* Governance rules */}
      {governanceState && (
        <CollapsibleSection
          title="Governance"
          expanded={rulesExpanded}
          onToggle={() => setRulesExpanded(!rulesExpanded)}
          helpText="The active governance rules: who governs this party, how many of them must confirm an action, and who else may propose one."
          badge={
            <Chip
              label={governanceState.members.length}
              size="small"
              sx={{ ml: 1 }}
            />
          }
        >
          <GovernanceRules
            state={governanceState}
            memberPartyId={authStatus?.member_party_id}
          />
        </CollapsibleSection>
      )}

      {/* Rewards */}
      {canRead && (
        <CollapsibleSection
          title="Rewards"
          expanded={rewardsExpanded}
          onToggle={() => setRewardsExpanded(!rewardsExpanded)}
          helpText="Reward coupon reassignment for this party: whether this participant has the rewards DAR, the active delegation, and whether this node is an assigner."
        >
          <RewardStatus
            status={rewards?.status ?? null}
            error={rewards?.error ?? null}
          />
        </CollapsibleSection>
      )}

      {/* Participants */}
      <CollapsibleSection
        title="Participants"
        expanded={participantsExpanded}
        onToggle={() => setParticipantsExpanded(!participantsExpanded)}
        helpText="Canton participants hosting this party, one per row with its permission level. Owners of the party's namespace that do not host it follow the hosts, marked Owner only."
        badge={
          <>
            <Chip
              label={party.participants.length + nonHostingOwners.length}
              size="small"
              sx={{ ml: 1 }}
            />
            <Tooltip title="Add member">
              <span>
                <IconButton
                  size="small"
                  sx={{ ml: 0.5 }}
                  onClick={(e) => {
                    // Don't toggle the collapsible section.
                    e.stopPropagation();
                    setAddDialogOpen(true);
                  }}
                  disabled={!ADMIN_ACCESS}
                >
                  <PersonAddIcon fontSize="small" />
                </IconButton>
              </span>
            </Tooltip>
          </>
        }
      >
        <PartyParticipantsTable
          party={party}
          nonHostingOwners={nonHostingOwners}
          selfParticipantId={selfParticipantId}
          peers={peers}
          sharedCounts={sharedCounts}
          onOpenPeerConfig={onOpenPeerConfig}
          onShowSharedParties={onShowSharedParties}
          handleKickClick={handleKickClick}
          handleKickOwnerClick={handleKickOwnerClick}
        />
      </CollapsibleSection>

      {/* Contracts */}
      {party.contracts && party.contracts.length > 0 && (
        <CollapsibleSection
          title="Contracts"
          expanded={contractsExpanded}
          onToggle={() => setContractsExpanded(!contractsExpanded)}
          helpText="Daml contracts associated with the party — typically governance rules, registrar services, etc."
          badge={
            <Chip label={party.contracts.length} size="small" sx={{ ml: 1 }} />
          }
        >
          <PartyContractsTable
            canScrollUp={canScrollUp}
            canScrollDown={canScrollDown}
            contractsScrollRef={contractsScrollRef}
            contractsPaging={contractsPaging}
          />
        </CollapsibleSection>
      )}

      {/* Holdings */}
      {canRead && (
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
      {canRead && (
        <CollapsibleSection
          title="Audit Trail"
          expanded={governanceExpanded}
          onToggle={() => setGovernanceExpanded(!governanceExpanded)}
          helpText="On-chain history of governance actions and proposals for this party."
          badge={
            <>
              {auditTrailCount > 0 && (
                <Chip
                  label={`${auditTrailCount}${auditTrailHasMore ? "+" : ""}`}
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
            onCountChange={onAuditTrailCountChange}
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
        participantUid={
          selectedOwner ? selectedOwner.participant_uid : selectedParticipant
        }
        participantOwnerKey={
          selectedOwner
            ? selectedOwner.owner_key
            : party.participants.find(
                (p) => p.participant_uid === selectedParticipant,
              )?.owner_key
        }
        hosting={selectedOwner === null}
        currentThreshold={party.threshold}
        currentOwnerCount={party.owners.length}
        currentHostCount={party.participants.length}
      />

      <AddPartyDialog
        open={addDialogOpen}
        onClose={() => setAddDialogOpen(false)}
        onAddComplete={() => {
          onRefresh();
          onNavigateToNotifications();
        }}
        partyId={party.party_id}
        participantUids={party.participants.map((p) => p.participant_uid)}
        currentThreshold={party.threshold}
        currentOwnerCount={party.owners.length}
      />

      <ChangeThresholdDialog
        open={changeThresholdDialogOpen}
        onClose={() => setChangeThresholdDialogOpen(false)}
        onComplete={() => {
          onRefresh();
          onNavigateToNotifications();
        }}
        partyId={party.party_id}
        currentThreshold={party.threshold}
        currentOwnerCount={party.owners.length}
        currentConfirmationThreshold={party.confirmation_threshold}
        currentHostCount={party.participants.length}
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
