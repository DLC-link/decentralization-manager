import {
  Alert,
  Autocomplete,
  Box,
  CircularProgress,
  IconButton,
  TextField,
  Tooltip,
} from "@mui/material";
import RefreshIcon from "@mui/icons-material/Refresh";
import { formatDecimal } from "../../../paidCredential";
import { TextHelp } from "../../FieldHelp";
import { ContractSelect } from "../fields/ContractSelect";
import { CredentialOfferPicker } from "../fields/CredentialOfferPicker";
import { HelpTextField } from "../fields/HelpTextField";
import { AmuletRulesCidField } from "./AmuletRulesCidField";
import type { ProposalState } from "./useProposalState";

interface Props {
  state: ProposalState;
}

/** The Offer and Accept Free and Paid Credential forms. */
export const CredentialForms = ({ state }: Props) => {
  const {
    proposalType,
    proposalDescription,
    setProposalDescription,
    proposalHolder,
    setProposalHolder,
    proposalUserServiceCid,
    setProposalUserServiceCid,
    proposalCredentialId,
    setProposalCredentialId,
    proposalCredentialClaimsText,
    setProposalCredentialClaimsText,
    proposalCredentialOfferCid,
    setProposalCredentialOfferCid,
    proposalFeePerDayUsd,
    setProposalFeePerDayUsd,
    proposalBillingPeriodMinutes,
    setProposalBillingPeriodMinutes,
    proposalDepositTargetAmountUsd,
    setProposalDepositTargetAmountUsd,
    proposalHolderActivityWeight,
    setProposalHolderActivityWeight,
    proposalDepositInitialAmountUsd,
    setProposalDepositInitialAmountUsd,
    proposalDepositAmuletCids,
    setProposalDepositAmuletCids,
    proposalOpenMiningRoundCid,
    setProposalOpenMiningRoundCid,
    proposalFeaturedAppRightCid,
    setProposalFeaturedAppRightCid,
    openMiningRound,
    openMiningRoundError,
    openMiningRoundLoading,
    userServices,
    credentialOffersLoading,
    ownAmulets,
    ownAmuletsLoading,
    servicesLoading,
    acceptableCredentialOffers,
    acceptablePaidCredentialOffers,
    chosenPaidOffer,
    chosenDepositUsd,
    fetchOpenMiningRound,
  } = state;

  // UserService select shared by the credential proposal forms.
  const renderUserServiceSelect = (help: string) => (
    <ContractSelect
      label="UserService Contract ID"
      help={help}
      value={proposalUserServiceCid}
      onChange={setProposalUserServiceCid}
      items={userServices}
      loading={servicesLoading}
      loadingText="Loading services…"
      emptyText="No user services found"
    />
  );

  return (
    <>
      {(proposalType === "offer_free_credential" ||
        proposalType === "offer_paid_credential") && (
        <>
          {renderUserServiceSelect(
            "Contract id of this governance party's UserService, used to issue the credential offer.",
          )}
          <HelpTextField
            size="small"
            label="Holder Party"
            value={proposalHolder}
            onChange={(e) => setProposalHolder(e.target.value)}
            fullWidth
            required
            help="Party id that will receive the credential offer."
          />
          <HelpTextField
            size="small"
            label="Credential ID"
            value={proposalCredentialId}
            onChange={(e) => setProposalCredentialId(e.target.value)}
            fullWidth
            required
            help="Unique identifier for this credential (free-form string)."
          />
          <HelpTextField
            size="small"
            label="Description"
            value={proposalDescription}
            onChange={(e) => setProposalDescription(e.target.value)}
            fullWidth
            required
            help="Free-form human-readable description of what this credential certifies."
          />
          <HelpTextField
            size="small"
            label="Claims (one per line: subject,property,value)"
            value={proposalCredentialClaimsText}
            onChange={(e) => setProposalCredentialClaimsText(e.target.value)}
            fullWidth
            multiline
            minRows={2}
            maxRows={6}
            helperText='Each line: "<subject>,<property>,<value>"'
            help="Claims baked into the credential. One per line, each formatted as subject,property,value."
            helpLabel="Claims"
          />
          {proposalType === "offer_paid_credential" && (
            <>
              <HelpTextField
                size="small"
                label="Fee per Day (USD)"
                value={proposalFeePerDayUsd}
                onChange={(e) => setProposalFeePerDayUsd(e.target.value)}
                fullWidth
                required
                help="The credential's daily fee in USD. Billing takes it from the holder's deposit."
              />
              <HelpTextField
                size="small"
                label="Billing Period (minutes)"
                value={proposalBillingPeriodMinutes}
                onChange={(e) => setProposalBillingPeriodMinutes(e.target.value)}
                fullWidth
                required
                help="Minutes between fee charges. Each charge covers the time since the last one."
              />
              <HelpTextField
                size="small"
                label="Deposit Target (USD)"
                value={proposalDepositTargetAmountUsd}
                onChange={(e) => setProposalDepositTargetAmountUsd(e.target.value)}
                fullWidth
                required
                help="The deposit in USD that the holder keeps locked to cover fees. Top-ups refill the deposit to this amount."
              />
              <HelpTextField
                size="small"
                label="Holder Activity Weight"
                value={proposalHolderActivityWeight}
                onChange={(e) => setProposalHolderActivityWeight(e.target.value)}
                fullWidth
                help="The holder's share of the activity reward, from 0 to 1. The issuer gets the rest. Leave empty for 0."
              />
              <HelpTextField
                size="small"
                label="Initial Deposit (USD)"
                value={proposalDepositInitialAmountUsd}
                onChange={(e) => setProposalDepositInitialAmountUsd(e.target.value)}
                fullWidth
                help="The USD amount the holder locks on accept. Leave empty to lock the Deposit Target."
              />
            </>
          )}
        </>
      )}

      {proposalType === "accept_free_credential" && (
        <>
          {renderUserServiceSelect(
            "Contract id of this governance party's UserService, used to accept the credential offer.",
          )}
          <CredentialOfferPicker
            value={proposalCredentialOfferCid}
            setValue={setProposalCredentialOfferCid}
            offers={acceptableCredentialOffers}
            kind="free"
            loading={credentialOffersLoading}
          />
        </>
      )}

      {proposalType === "accept_paid_credential" && (
        <>
          {renderUserServiceSelect(
            "Contract id of this governance party's UserService, used to accept the credential offer.",
          )}
          <CredentialOfferPicker
            value={proposalCredentialOfferCid}
            setValue={setProposalCredentialOfferCid}
            offers={acceptablePaidCredentialOffers}
            kind="paid"
            loading={credentialOffersLoading}
          />
          {chosenPaidOffer?.billing_params && chosenDepositUsd ? (
            <Alert severity="info">
              This accept locks a deposit of {formatDecimal(chosenDepositUsd)} USD, and
              the vote shows that amount. The offer's deposit target is{" "}
              {formatDecimal(chosenPaidOffer.billing_params.deposit_target_amount_usd)} USD.
              Its fee is {formatDecimal(chosenPaidOffer.billing_params.fee_per_day_usd)} USD
              a day, billed every {chosenPaidOffer.billing_params.billing_period_minutes}{" "}
              minutes.
            </Alert>
          ) : (
            proposalCredentialOfferCid && (
              <Alert severity="warning">
                This offer is not in the list, so the vote will not show its deposit.
              </Alert>
            )
          )}
          <Autocomplete
            size="small"
            multiple
            freeSolo
            options={ownAmulets}
            value={proposalDepositAmuletCids}
            loading={ownAmuletsLoading}
            onChange={(_event, values) => {
              setProposalDepositAmuletCids(
                values.map((value) =>
                  typeof value === "string" ? value : value.contract_id,
                ),
              );
            }}
            getOptionLabel={(option) => {
              // Selected values are stored as bare cids; label them
              // with the amount when the Amulet is known.
              const cid = typeof option === "string" ? option : option.contract_id;
              const known =
                typeof option === "string"
                  ? ownAmulets.find((a) => a.contract_id === option)
                  : option;
              return known ? `${known.amount} CC (…${cid.slice(-8)})` : cid;
            }}
            isOptionEqualToValue={(option, value) =>
              typeof value === "string"
                ? option.contract_id === value
                : option.contract_id === value.contract_id
            }
            renderInput={(params) => (
              <TextField
                {...params}
                label={
                  <TextHelp text="Unlocked Amulets of this governance party. Together they must cover the offer's deposit.">
                    Deposit Amulets
                  </TextHelp>
                }
                helperText={
                  ownAmuletsLoading
                    ? "Loading Amulets…"
                    : ownAmulets.length === 0
                      ? "No Amulets visible — paste a contract id directly if you have one"
                      : "Pick one or more Amulets"
                }
              />
            )}
          />
          <AmuletRulesCidField state={state} />
          <Box sx={{ display: "flex", gap: 1, alignItems: "center" }}>
            <HelpTextField
              size="small"
              label="Open Mining Round CID"
              value={proposalOpenMiningRoundCid}
              onChange={(e) => setProposalOpenMiningRoundCid(e.target.value)}
              fullWidth
              required
              helperText={
                openMiningRoundLoading
                  ? "Reading the open round from scan…"
                  : openMiningRound && openMiningRound.contract_id === proposalOpenMiningRoundCid
                    ? `Round ${openMiningRound.round}, open until ${new Date(openMiningRound.target_closes_at).toLocaleTimeString()}`
                    : undefined
              }
              help="Contract id of an OpenMiningRound that is open now. DecMan fills in the newest one from scan. You can paste another one."
            />
            <Tooltip title="Refresh">
              <span>
                <IconButton
                  size="small"
                  onClick={fetchOpenMiningRound}
                  disabled={openMiningRoundLoading}
                  aria-label="Refresh the open mining round"
                >
                  {openMiningRoundLoading ? <CircularProgress size={20} /> : <RefreshIcon />}
                </IconButton>
              </span>
            </Tooltip>
          </Box>
          {openMiningRoundError && (
            <Alert severity="warning">{openMiningRoundError}</Alert>
          )}
          <HelpTextField
            size="small"
            label="Featured App Right CID"
            value={proposalFeaturedAppRightCid}
            onChange={(e) => setProposalFeaturedAppRightCid(e.target.value)}
            fullWidth
            help="Optional FeaturedAppRight of the credential issuer. The deposit transfer is unfeatured, so you can leave this empty."
          />
          <Alert severity="info">
            The proposal pins these Amulets and this open mining round.
            The round closes about 20 minutes after it opens. Confirm
            and execute before then, or propose again.
          </Alert>
        </>
      )}
    </>
  );
};
