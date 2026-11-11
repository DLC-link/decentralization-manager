import { Typography, Alert, Box, Card, CardActionArea, CardContent, Tooltip } from "@mui/material";
import AddIcon from "@mui/icons-material/Add";
import AccountBalanceIcon from "@mui/icons-material/AccountBalance";
import GavelIcon from "@mui/icons-material/Gavel";
import HandymanIcon from "@mui/icons-material/Handyman";
import LockIcon from "@mui/icons-material/Lock";
import type { ContractType } from "./contractPresets";

// Plugin card with optional tooltip and enabled/disabled visual state
const PluginCard = ({
  icon,
  label,
  description,
  onClick,
  tooltip,
  enabled = false,
}: {
  icon: React.ReactNode;
  label: string;
  description: string;
  onClick?: () => void;
  tooltip?: string;
  enabled?: boolean;
}) => {
  const card = (
    <Card
      sx={{
        flex: 1,
        border: 1,
        borderColor: "divider",
        opacity: enabled ? 1 : 0.5,
        "&:hover": onClick ? { borderColor: "primary.main" } : {},
      }}
    >
      <CardActionArea
        disabled={!onClick}
        onClick={onClick}
        sx={{ p: 2, height: "100%" }}
      >
        <CardContent sx={{ textAlign: "center" }}>
          {icon}
          <Typography variant="h6">{label}</Typography>
          <Typography variant="body2" color="text.secondary">
            {description}
          </Typography>
        </CardContent>
      </CardActionArea>
    </Card>
  );
  return tooltip ? (
    <Tooltip title={tooltip} arrow>
      {card}
    </Tooltip>
  ) : (
    card
  );
};

// Contract type selection screen
interface ContractTypeSelectionProps {
  onSelect: (type: ContractType) => void;
  isGovernanceCoreDeployed: boolean;
  isGovernanceCoreDarUploaded: boolean;
  isTokenCustodyDarUploaded: boolean;
  isUtilityCredentialPluginDarUploaded: boolean;
  /** Why the vetted package list failed to load, or null when it loaded. */
  vettedPackagesError: string | null;
}

const VETTED_PACKAGES_UNREADABLE = "Could not load the vetted package list";

/** A failed read must not read as a missing DAR. */
const missingDarHint = (dar: string, vettedPackagesError: string | null) =>
  vettedPackagesError ? VETTED_PACKAGES_UNREADABLE : `Upload the ${dar} DAR first`;

const ContractTypeSelection = ({
  onSelect,
  isGovernanceCoreDeployed,
  isGovernanceCoreDarUploaded,
  isTokenCustodyDarUploaded,
  isUtilityCredentialPluginDarUploaded,
  vettedPackagesError,
}: ContractTypeSelectionProps) => {
  return (
    <Box sx={{ display: "flex", flexDirection: "column", gap: 2 }}>
      {vettedPackagesError && (
        <Alert severity="warning">
          {VETTED_PACKAGES_UNREADABLE}: {vettedPackagesError}. The cards below
          cannot tell whether a DAR is uploaded. Close and reopen this dialog
          to try again.
        </Alert>
      )}
      {/* Deploy Governance Core — shown only when not yet deployed */}
      {!isGovernanceCoreDeployed && (
        <>
          <Typography variant="body2" color="text.secondary">
            Deploy the core governance contract for this decentralized party.
          </Typography>
          <Box sx={{ display: "flex", gap: 2, mt: 1 }}>
            <Tooltip
              title={
                isGovernanceCoreDarUploaded
                  ? ""
                  : missingDarHint("governance-core", vettedPackagesError)
              }
              arrow
            >
              <Card
                sx={{
                  flex: 1,
                  maxWidth: 280,
                  border: 1,
                  borderColor: "divider",
                  opacity: isGovernanceCoreDarUploaded ? 1 : 0.5,
                  "&:hover": isGovernanceCoreDarUploaded
                    ? { borderColor: "primary.main" }
                    : {},
                }}
              >
                <CardActionArea
                  onClick={() => onSelect("governance-core")}
                  disabled={!isGovernanceCoreDarUploaded}
                  sx={{ p: 2, height: "100%" }}
                >
                  <CardContent sx={{ textAlign: "center" }}>
                    <GavelIcon
                      sx={{ fontSize: 48, color: "primary.main", mb: 1 }}
                    />
                    <Typography variant="h6">Governance Core</Typography>
                    <Typography variant="body2" color="text.secondary">
                      Deploy core governance rules contract
                    </Typography>
                  </CardContent>
                </CardActionArea>
              </Card>
            </Tooltip>
          </Box>
        </>
      )}

      {/* Plugins — shown only when governance core is deployed */}
      {isGovernanceCoreDeployed && (
        <>
          <Typography variant="body2" color="text.secondary">
            Governance Core is deployed. Available plugins:
          </Typography>
          <Box
            sx={{
              display: "grid",
              gridTemplateColumns: "repeat(2, 1fr)",
              gap: 2,
              mt: 1,
            }}
          >
            <PluginCard
              icon={
                <LockIcon sx={{ fontSize: 48, color: "primary.main", mb: 1 }} />
              }
              label="Token Custody"
              description="Receive, transfer, and manage tokens via governance"
              enabled={isTokenCustodyDarUploaded}
              tooltip={
                isTokenCustodyDarUploaded
                  ? undefined
                  : missingDarHint("governance-token-custody", vettedPackagesError)
              }
            />
            <PluginCard
              icon={
                <AccountBalanceIcon
                  sx={{ fontSize: 48, color: "primary.main", mb: 1 }}
                />
              }
              label="CBTC"
              description="Bitcoin-backed deposit and withdrawal management"
              onClick={() => onSelect("cbtc")}
              tooltip="Coming soon"
            />
            <PluginCard
              icon={
                <HandymanIcon
                  sx={{ fontSize: 48, color: "primary.main", mb: 1 }}
                />
              }
              label="Utility"
              description="Registry services and verifiable credentials"
              tooltip={
                isUtilityCredentialPluginDarUploaded
                  ? "Credential plugin DAR uploaded — propose Offer/Accept Free credential actions from the Governance section."
                  : "Coming soon"
              }
            />
            <PluginCard
              icon={
                <AddIcon sx={{ fontSize: 48, color: "primary.main", mb: 1 }} />
              }
              label="Add Plugin"
              description="Upload a custom plugin DAR from the Packages tab"
              tooltip="Go to the Packages tab to upload DARs"
            />
          </Box>
        </>
      )}
    </Box>
  );
};

export { ContractTypeSelection };
