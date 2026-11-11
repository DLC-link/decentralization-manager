import { useCallback, useState } from "react";
import {
  Autocomplete,
  Box,
  Typography,
  Button,
  Alert,
  Collapse,
  TextField,
  Select,
  MenuItem,
  FormControl,
  InputLabel,
  Divider,
  Checkbox,
  FormControlLabel,
  ListSubheader,
} from "@mui/material";
import { API_BASE } from "../../../constants";
import { authenticatedFetch } from "../../../api";
import { } from "../../../rewardBeneficiaries";
import { TextHelp } from "../../FieldHelp";
import { ContractSelect } from "../fields/ContractSelect";
import { } from "../fields/CredentialOfferPicker";
import { HelpTextField } from "../fields/HelpTextField";
import { RequirementRows, type RequirementRow } from "../fields/RequirementRows";
import { SubmitButton } from "../fields/SubmitButton";
import {
  buildRequirements,
  credentialValidity,
  parsePartyLines,
  requiredClaimsText,
} from "../proposalParse";
import { TokenCustodyForms } from "./TokenCustodyForms";
import {
  buildSetupCcPreapproval,
  buildSetupTokenPreapproval,
  buildGenericVote,
} from "./tokenCustodyPayloads";
import { TransferForm } from "./TransferForm";
import { buildTransfer, buildAcceptTransfer } from "./transferPayloads";
import { } from "./AmuletRulesCidField";
import { UtilityOnboardingForms } from "./UtilityOnboardingForms";
import {
  buildSetupUtility,
  buildCreateProviderServiceRequest,
  buildCreateUserServiceRequest,
  buildSetEnableResultContracts,
  buildCreateDelegatedBatchedMarkersProxy,
  buildRequestDevNetFeaturedAppRight,
} from "./utilityOnboardingPayloads";
import { RewardBeneficiariesForm } from "./RewardBeneficiariesForm";
import { buildSetProviderAppRewardBeneficiaries } from "./rewardBeneficiariesPayload";
import { CouponDelegationForms } from "./CouponDelegationForms";
import {
  buildSetupCouponReassignmentDelegation,
  buildRevokeCouponReassignmentDelegation,
} from "./couponDelegationPayloads";
import { MintingDelegationForms } from "./MintingDelegationForms";
import {
  buildSetupMintingDelegation,
  buildAcceptExternalPartySetup,
} from "./mintingDelegationPayloads";
import { CredentialForms } from "./CredentialForms";
import {
  buildOfferFreeCredential,
  buildAcceptFreeCredential,
  buildOfferPaidCredential,
  buildAcceptPaidCredential,
} from "./credentialPayloads";
import { TokenRequestForm } from "./TokenRequestForm";
import { buildAcceptMintRequest, buildAcceptBurnRequest } from "./tokenRequestPayloads";
import { PROPOSAL_MENU, type FormProposalType } from "./proposalMenu";
import type { ProposalState } from "./useProposalState";
import type {
  Network,
  PartyCredentialRequirement,
  ProposalType,
  ProposeActionRequest,
} from "../../../types";

interface ProposalFormProps {
  /** The form fields and lookup lists, from useProposalState. */
  state: ProposalState;
  rulesContractId: string;
  network?: Network;
  setError: (error: string | null) => void;
  fetchGovernance: () => Promise<void>;
  onAfterAction?: () => void;
  onProposalCreated?: () => void;
  submitPortalEl?: HTMLElement | null;
}

/** The new-proposal form: the type menu, the fields of the chosen type and submit. */
export const ProposalForm = ({
  state,
  rulesContractId,
  network,
  setError,
  fetchGovernance,
  onAfterAction,
  onProposalCreated,
  submitPortalEl,
}: ProposalFormProps) => {
  const {
    partyId,
    proposalType,
    setProposalType,
    proposalProvider,
    setProposalProvider,
    proposalCredentialValidFrom,
    setProposalCredentialValidFrom,
    proposalCredentialValidUntil,
    setProposalCredentialValidUntil,
    proposalOperator,
    setProposalOperator,
    proposalProviderServiceCid,
    setProposalProviderServiceCid,
    proposalInstrumentIdText,
    setProposalInstrumentIdText,
    proposalCreateTransferRule,
    setProposalCreateTransferRule,
    proposalCreateAllocationFactory,
    setProposalCreateAllocationFactory,
    proposalAdditionalIdentifiers,
    setProposalAdditionalIdentifiers,
    proposalInstrumentConfigurationCid,
    setProposalInstrumentConfigurationCid,
    proposalRegistrarServiceCid,
    setProposalRegistrarServiceCid,
    proposalRegistrarServiceRequestCid,
    setProposalRegistrarServiceRequestCid,
    proposalProviderConfigurationCid,
    setProposalProviderConfigurationCid,
    proposalRegistrarRequirements,
    setProposalRegistrarRequirements,
    proposalHolderRequirements,
    setProposalHolderRequirements,
    proposalIssuerRequirements,
    setProposalIssuerRequirements,
    proposalInitialInstrumentIssuersText,
    setProposalInitialInstrumentIssuersText,
    proposalInstrumentIssuersText,
    setProposalInstrumentIssuersText,
    proposalOffboardRows,
    setProposalOffboardRows,
    providerServices,
    availableCredentials,
    credentialsLoading,
    registrarServiceRequestsLoading,
    registrarServicesLoading,
    providerConfigurations,
    providerConfigurationsLoading,
    instrumentsLoading,
    servicesLoading,
    acceptableRegistrarServiceRequests,
    ownRegistrarServices,
    ownInstruments,
    configurationToUpdate,
    configurationToUpdateUnreadable,
    resetProposalForm,
    beneficiaryProblem,
    blockListProblem,
  } = state;
  const [proposalLoading, setProposalLoading] = useState(false);

  // The update replaces every field, so picking a configuration fills the
  // form with its current values. A field left empty is cleared.
  const selectConfigurationToUpdate = (cid: string) => {
    setProposalInstrumentConfigurationCid(cid);
    const instrument = ownInstruments.find((inst) => inst.contract_id === cid);
    const current = instrument?.configuration;
    const toRows = (requirements: PartyCredentialRequirement[]): RequirementRow[] =>
      requirements.map((requirement) => ({
        issuer: requirement.issuer,
        claimsText: requiredClaimsText(requirement.required_claims),
        claims: requirement.required_claims,
      }));
    setProposalInstrumentIdText(instrument?.instrument_id ?? "");
    setProposalAdditionalIdentifiers(current?.additional_identifiers ?? []);
    setProposalIssuerRequirements(toRows(current?.issuer_requirements ?? []));
    setProposalHolderRequirements(toRows(current?.holder_requirements ?? []));
  };

  // Candidate credentials for one offboard row: self-issued by this governance
  // party, carrying claims that all name the row's party, and tagged as an
  // instrument-issuer credential so a dual-role decparty's registrar
  // credentials stay out.
  const offboardableCredentialsFor = useCallback(
    (party: string) =>
      availableCredentials.filter(
        (c) =>
          c.issuer === partyId &&
          c.holder === partyId &&
          c.claims.length > 0 &&
          c.claims.every((cl) => cl.subject === party) &&
          c.credential_id.includes("-instrument-issuer-credential/"),
      ),
    [availableCredentials, partyId],
  );

  const handleSubmitProposal = async () => {
    if (!rulesContractId) return;
    setProposalLoading(true);
    setError(null);

    try {
      let proposal: ProposalType;
      switch (proposalType) {
        case "setup_cc_preapproval":
          proposal = buildSetupCcPreapproval(state);
          break;
        case "setup_token_preapproval":
          proposal = buildSetupTokenPreapproval(state);
          break;
        case "transfer":
          proposal = buildTransfer(state);
          break;
        case "accept_transfer":
          proposal = buildAcceptTransfer(state);
          break;
        case "generic_vote":
          proposal = buildGenericVote(state);
          break;
        case "setup_utility":
          proposal = buildSetupUtility(state);
          break;
        case "create_provider_service_request":
          proposal = buildCreateProviderServiceRequest(state);
          break;
        case "create_user_service_request":
          proposal = buildCreateUserServiceRequest(state);
          break;
        case "set_provider_app_reward_beneficiaries":
          proposal = buildSetProviderAppRewardBeneficiaries(state);
          break;
        case "setup_coupon_reassignment_delegation":
          proposal = buildSetupCouponReassignmentDelegation(state);
          break;
        case "revoke_coupon_reassignment_delegation":
          proposal = buildRevokeCouponReassignmentDelegation(state);
          break;
        case "set_enable_result_contracts":
          proposal = buildSetEnableResultContracts(state);
          break;
        case "create_delegated_batched_markers_proxy":
          proposal = buildCreateDelegatedBatchedMarkersProxy(state);
          break;
        case "request_dev_net_featured_app_right":
          proposal = buildRequestDevNetFeaturedAppRight(state);
          break;
        case "setup_minting_delegation":
          proposal = buildSetupMintingDelegation(state);
          break;
        case "accept_external_party_setup":
          proposal = buildAcceptExternalPartySetup(state);
          break;
        case "accept_mint_request":
          proposal = buildAcceptMintRequest(state);
          break;
        case "accept_burn_request":
          proposal = buildAcceptBurnRequest(state);
          break;
        case "offer_free_credential":
          proposal = buildOfferFreeCredential(state);
          break;
        case "accept_free_credential":
          proposal = buildAcceptFreeCredential(state);
          break;
        case "offer_paid_credential":
          proposal = buildOfferPaidCredential(state);
          break;
        case "accept_paid_credential":
          proposal = buildAcceptPaidCredential(state);
          break;
        case "create_provider_configuration":
          proposal = {
            type: "create_provider_configuration",
            provider_service_cid: proposalProviderServiceCid,
            registrar_requirements: buildRequirements(
              proposalRegistrarRequirements,
              "Registrar",
            ),
            holder_requirements: buildRequirements(
              proposalHolderRequirements,
              "Holder",
            ),
          };
          break;
        case "create_registrar_service_request":
          proposal = {
            type: "create_registrar_service_request",
            operator: proposalOperator,
            provider: proposalProvider,
            create_transfer_rule: proposalCreateTransferRule,
            create_allocation_factory: proposalCreateAllocationFactory,
          };
          break;
        case "onboard_registrar":
          proposal = {
            type: "onboard_registrar",
            provider_service_cid: proposalProviderServiceCid,
            registrar_service_request_cid: proposalRegistrarServiceRequestCid,
            provider_configuration_cid: proposalProviderConfigurationCid,
          };
          break;
        case "provision_instrument":
          proposal = {
            type: "provision_instrument",
            registrar_service_cid: proposalRegistrarServiceCid,
            instrument_id_text: proposalInstrumentIdText,
            // No identifier editor, matching the Setup Utility form; extra
            // identifiers go through the API directly.
            additional_identifiers: [],
            issuer_requirements: buildRequirements(
              proposalIssuerRequirements,
              "Issuer",
            ),
            holder_requirements: buildRequirements(
              proposalHolderRequirements,
              "Holder",
            ),
            initial_instrument_issuers: parsePartyLines(
              proposalInitialInstrumentIssuersText,
            ),
            ...credentialValidity(proposalCredentialValidFrom, proposalCredentialValidUntil),
          };
          break;
        case "create_transfer_rule":
          proposal = {
            type: "create_transfer_rule",
            registrar_service_cid: proposalRegistrarServiceCid,
          };
          break;
        case "update_instrument_configuration":
          if (blockListProblem) throw new Error(blockListProblem);
          proposal = {
            type: "update_instrument_configuration",
            registrar_service_cid: proposalRegistrarServiceCid,
            instrument_configuration_cid: proposalInstrumentConfigurationCid,
            instrument_id_text: proposalInstrumentIdText,
            additional_identifiers: proposalAdditionalIdentifiers.map((row, idx) => {
              // The id and scheme go out verbatim, so an unchanged identifier
              // stays unchanged. The ledger requires only a nonempty id; an
              // empty scheme is valid.
              const identifier = {
                source: row.source.trim(),
                id: row.id,
                scheme: row.scheme,
              };
              if (!identifier.source || !identifier.id) {
                throw new Error(
                  `Additional identifier ${idx + 1}: source and id are required`,
                );
              }
              return identifier;
            }),
            issuer_requirements: buildRequirements(proposalIssuerRequirements, "Issuer"),
            holder_requirements: buildRequirements(proposalHolderRequirements, "Holder"),
            // The current flag, so the update keeps it.
            use_block_list: configurationToUpdate?.check_block_list === false ? false : null,
          };
          break;
        case "onboard_instrument_issuers":
          proposal = {
            type: "onboard_instrument_issuers",
            instrument_configuration_cid: proposalInstrumentConfigurationCid,
            instrument_issuers: parsePartyLines(proposalInstrumentIssuersText),
            ...credentialValidity(proposalCredentialValidFrom, proposalCredentialValidUntil),
          };
          break;
        case "offboard_instrument_issuers":
          proposal = {
            type: "offboard_instrument_issuers",
            instrument_issuers: proposalOffboardRows.map((row) => ({
              instrument_issuer: row.party,
              credential_cids: row.cids,
            })),
          };
          break;
      }

      const request: ProposeActionRequest = {
        party_id: partyId,
        rules_contract_id: rulesContractId,
        proposal,
      };

      const res = await authenticatedFetch(`${API_BASE}/governance/propose`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(request),
      });

      if (!res.ok) {
        const errData = await res.json().catch(() => ({}));
        throw new Error(errData.error || "Failed to create proposal");
      }

      // Clear fields and let the host close the dialog. The created proposal
      // shows up in the notification queue — no separate success message
      // needed.
      resetProposalForm();
      await fetchGovernance();
      onAfterAction?.();
      onProposalCreated?.();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to create proposal");
    } finally {
      setProposalLoading(false);
    }
  };

  return (
    <Box>
      <Collapse in>
        <Box sx={{ display: "flex", flexDirection: "column", gap: 1.5 }}>
          <FormControl size="small" fullWidth>
            <InputLabel>
              <TextHelp text="What kind of proposal to create. The form fields below adapt to the selected type.">
                Proposal Type
              </TextHelp>
            </InputLabel>
            <Select
              value={proposalType}
              label="Proposal Type"
              onChange={(e) => {
                const next = e.target.value as FormProposalType;
                if (next !== proposalType) resetProposalForm(next);
                setProposalType(next);
              }}
            >
              {PROPOSAL_MENU.map((entry, idx) => {
                switch (entry.kind) {
                  case "group":
                    return (
                      <ListSubheader key={idx} sx={{ color: "primary.main", fontWeight: 600 }}>
                        {entry.label}
                      </ListSubheader>
                    );
                  case "step":
                    return (
                      <ListSubheader key={idx} sx={{ fontStyle: "italic", lineHeight: 1.5, pl: 4 }}>
                        {entry.label}
                      </ListSubheader>
                    );
                  case "type":
                    return (
                      <MenuItem key={idx} value={entry.value}>
                        {entry.label}
                      </MenuItem>
                    );
                  case "divider":
                    return <Divider key={idx} />;
                }
              })}
            </Select>
          </FormControl>

          <Divider />

          <UtilityOnboardingForms state={state} network={network} />

          <TokenCustodyForms state={state} />

          <TransferForm state={state} />

          <CouponDelegationForms state={state} />

          <RewardBeneficiariesForm state={state} />

          <MintingDelegationForms state={state} />

          <TokenRequestForm state={state} />

          <CredentialForms state={state} />

          {proposalType === "create_provider_configuration" && (
            <>
              <ContractSelect
                label="ProviderService"
                help="ProviderService contract of this provider decparty. Its provider must be the governance party."
                value={proposalProviderServiceCid}
                onChange={setProposalProviderServiceCid}
                items={providerServices}
                loading={servicesLoading}
                loadingText="Loading services…"
                emptyText='No ProviderService found — run "Create Provider Service Request" first'
              />
              <RequirementRows
                label="Registrar Requirements"
                help="Credential requirements a registrar must meet before the provider onboards it. Rows whose issuer is this governance party are minted automatically during Onboard Registrar."
                rows={proposalRegistrarRequirements}
                setRows={setProposalRegistrarRequirements}
                defaultIssuer={partyId}
              />
              <RequirementRows
                label="Holder Requirements"
                help="Credential requirements a token holder must meet on this provider's utility."
                rows={proposalHolderRequirements}
                setRows={setProposalHolderRequirements}
                defaultIssuer={partyId}
              />
            </>
          )}

          {proposalType === "create_registrar_service_request" && (
            <>
              <HelpTextField
                size="small"
                label="Operator Party"
                value={proposalOperator}
                onChange={(e) => setProposalOperator(e.target.value)}
                fullWidth
                required
                help="Party id of the utility operator the registrar service runs under."
              />
              <HelpTextField
                size="small"
                label="Provider Party"
                value={proposalProvider}
                onChange={(e) => setProposalProvider(e.target.value)}
                fullWidth
                required
                help="Party id of the provider decparty this request asks for registrar service. The provider accepts via Onboard Registrar."
              />
              <FormControlLabel
                control={<Checkbox size="small" checked={proposalCreateTransferRule} onChange={(e) => setProposalCreateTransferRule(e.target.checked)} />}
                label={
                  <TextHelp text="Also create a TransferRule contract during the accept, so holders can transfer this registrar's tokens without per-transfer governance.">
                    Create TransferRule
                  </TextHelp>
                }
              />
              <FormControlLabel
                control={<Checkbox size="small" checked={proposalCreateAllocationFactory} onChange={(e) => setProposalCreateAllocationFactory(e.target.checked)} />}
                label={
                  <TextHelp text="Also create an AllocationFactory contract during the accept, so this registrar's tokens can be allocated by external apps.">
                    Create AllocationFactory
                  </TextHelp>
                }
              />
            </>
          )}

          {proposalType === "onboard_registrar" && (
            <>
              <ContractSelect
                label="ProviderService"
                help="ProviderService contract of this provider decparty, used to accept the request."
                value={proposalProviderServiceCid}
                onChange={setProposalProviderServiceCid}
                items={providerServices}
                loading={servicesLoading}
                loadingText="Loading services…"
                emptyText='No ProviderService found — run "Create Provider Service Request" first'
              />
              <ContractSelect
                label="RegistrarServiceRequest"
                help="Pending RegistrarServiceRequest to accept. The request names the registrar to onboard; its provider must be this governance party."
                value={proposalRegistrarServiceRequestCid}
                onChange={setProposalRegistrarServiceRequestCid}
                items={acceptableRegistrarServiceRequests}
                loading={registrarServiceRequestsLoading}
                loadingText="Loading requests…"
                emptyText='No pending requests for this provider — the registrar runs "Create Registrar Service Request" first'
                rowLabel={(req) => <>{req.registrar.split("::")[0]} (…{req.contract_id.slice(-8)})</>}
              />
              <ContractSelect
                label="ProviderConfiguration"
                help="ProviderConfiguration holding the registrar requirements to mint and validate against."
                value={proposalProviderConfigurationCid}
                onChange={setProposalProviderConfigurationCid}
                items={providerConfigurations}
                loading={providerConfigurationsLoading}
                loadingText="Loading configurations…"
                emptyText='No ProviderConfiguration found — run "Create Provider Configuration" first'
              />
            </>
          )}

          {proposalType === "provision_instrument" && (
            <>
              <ContractSelect
                label="RegistrarService"
                help="RegistrarService contract of this registrar decparty, used to create the InstrumentConfiguration."
                value={proposalRegistrarServiceCid}
                onChange={setProposalRegistrarServiceCid}
                items={ownRegistrarServices}
                loading={registrarServicesLoading}
                loadingText="Loading services…"
                emptyText='No RegistrarService for this party — run "Onboard Registrar" first'
              />
              <HelpTextField
                size="small"
                label="Instrument ID"
                value={proposalInstrumentIdText}
                onChange={(e) => setProposalInstrumentIdText(e.target.value)}
                fullWidth
                required
                help={"Token name for the instrument this registrar will manage (e.g. \"cTM\")."}
              />
              <RequirementRows
                label="Issuer Requirements"
                help="Credential requirements an instrument issuer must meet. Rows whose issuer is this governance party are minted automatically for the initial issuers below."
                rows={proposalIssuerRequirements}
                setRows={setProposalIssuerRequirements}
                defaultIssuer={partyId}
              />
              <RequirementRows
                label="Holder Requirements"
                help="Credential requirements a holder of this instrument must meet."
                rows={proposalHolderRequirements}
                setRows={setProposalHolderRequirements}
                defaultIssuer={partyId}
              />
              <HelpTextField
                size="small"
                label="Initial Instrument Issuers (one party id per line)"
                value={proposalInitialInstrumentIssuersText}
                onChange={(e) => setProposalInitialInstrumentIssuersText(e.target.value)}
                fullWidth
                multiline
                minRows={2}
                maxRows={6}
                help="Parties credentialed as instrument issuers at provisioning, one party id per line. Leave empty to onboard issuers later."
                helpLabel="Initial Instrument Issuers"
              />
            </>
          )}

          {proposalType === "onboard_instrument_issuers" && (
            <>
              <ContractSelect
                label="InstrumentConfiguration"
                help="InstrumentConfiguration whose issuer requirements the new issuers are credentialed against."
                value={proposalInstrumentConfigurationCid}
                onChange={setProposalInstrumentConfigurationCid}
                items={ownInstruments}
                loading={instrumentsLoading}
                loadingText="Loading instruments…"
                emptyText='No instruments administered by this party — run "Provision Instrument" first'
                rowLabel={(inst) => <>{inst.instrument_id} ({inst.contract_id.slice(0, 8)}…)</>}
              />
              <HelpTextField
                size="small"
                label="Instrument Issuers (one party id per line)"
                value={proposalInstrumentIssuersText}
                onChange={(e) => setProposalInstrumentIssuersText(e.target.value)}
                fullWidth
                required
                multiline
                minRows={2}
                maxRows={6}
                help="Parties to credential as instrument issuers, one party id per line. At least one is required."
                helpLabel="Instrument Issuers"
              />
            </>
          )}

          {proposalType === "create_transfer_rule" && (
            <ContractSelect
              label="RegistrarService"
              help="RegistrarService contract of this registrar decparty. The new TransferRule belongs to it. Setup creates the first rule when asked to, so use this to add one later."
              value={proposalRegistrarServiceCid}
              onChange={setProposalRegistrarServiceCid}
              items={ownRegistrarServices}
              loading={registrarServicesLoading}
              loadingText="Loading services…"
              emptyText='No RegistrarService for this party — run "Setup Utility" or "Onboard Registrar" first'
            />
          )}

          {proposalType === "update_instrument_configuration" && (
            <>
              <Alert severity="info">
                This replaces every field of the configuration. Picking a
                configuration fills the form with its current values. A
                field you clear is cleared on the ledger. The reward
                beneficiaries stay as they are.
              </Alert>
              <ContractSelect
                label="RegistrarService"
                help="RegistrarService contract of this registrar decparty. The update runs through it."
                value={proposalRegistrarServiceCid}
                onChange={setProposalRegistrarServiceCid}
                items={ownRegistrarServices}
                loading={registrarServicesLoading}
                loadingText="Loading services…"
                emptyText='No RegistrarService for this party — run "Onboard Registrar" first'
              />
              <ContractSelect
                label="InstrumentConfiguration"
                help="The InstrumentConfiguration to replace. Execution archives it and creates a new one."
                value={proposalInstrumentConfigurationCid}
                onChange={selectConfigurationToUpdate}
                items={ownInstruments}
                loading={instrumentsLoading}
                loadingText="Loading instruments…"
                emptyText='No instruments administered by this party — run "Provision Instrument" first'
                rowLabel={(inst) => <>{inst.instrument_id} ({inst.contract_id.slice(0, 8)}…)</>}
              />
              {configurationToUpdateUnreadable && (
                <Alert severity="warning">
                  DecMan could not read this configuration&apos;s current
                  values. Enter every field by hand.
                </Alert>
              )}
              {blockListProblem && <Alert severity="warning">{blockListProblem}</Alert>}
              <HelpTextField
                size="small"
                label="Instrument ID"
                value={proposalInstrumentIdText}
                fullWidth
                disabled
                help="The instrument's id. Holdings name the instrument by it, so the update cannot change it."
              />
              <Typography variant="caption" color="text.secondary" sx={{ display: "block" }}>
                <TextHelp text="Other identifiers of the instrument, each with its source party, id and scheme.">
                  Additional Identifiers
                </TextHelp>
              </Typography>
              {proposalAdditionalIdentifiers.map((row, idx) => (
                <Box key={idx} sx={{ display: "flex", gap: 1, mb: 1, alignItems: "flex-start" }}>
                  {(["source", "id", "scheme"] as const).map((key) => (
                    <TextField
                      key={key}
                      label={`Identifier ${idx + 1} ${key === "source" ? "Source Party" : key === "id" ? "ID" : "Scheme"}`}
                      value={row[key]}
                      onChange={(e) => {
                        const updated = [...proposalAdditionalIdentifiers];
                        updated[idx] = { ...row, [key]: e.target.value };
                        setProposalAdditionalIdentifiers(updated);
                      }}
                      size="small"
                      sx={{ flex: key === "source" ? 2 : 1 }}
                    />
                  ))}
                  <Button
                    size="small"
                    color="error"
                    onClick={() =>
                      setProposalAdditionalIdentifiers(
                        proposalAdditionalIdentifiers.filter((_, i) => i !== idx),
                      )
                    }
                  >
                    Remove
                  </Button>
                </Box>
              ))}
              <Box>
                <Button
                  size="small"
                  onClick={() =>
                    setProposalAdditionalIdentifiers([
                      ...proposalAdditionalIdentifiers,
                      { source: partyId, id: "", scheme: "" },
                    ])
                  }
                >
                  Add Identifier
                </Button>
              </Box>
              <RequirementRows
                label="Issuer Requirements"
                help="Credential requirements an instrument issuer must meet."
                rows={proposalIssuerRequirements}
                setRows={setProposalIssuerRequirements}
                defaultIssuer={partyId}
              />
              <RequirementRows
                label="Holder Requirements"
                help="Credential requirements a holder of this instrument must meet."
                rows={proposalHolderRequirements}
                setRows={setProposalHolderRequirements}
                defaultIssuer={partyId}
              />
            </>
          )}

          {(proposalType === "provision_instrument" ||
            proposalType === "onboard_instrument_issuers") && (
            <>
              <HelpTextField
                size="small"
                label="Credentials Valid From (optional)"
                type="datetime-local"
                value={proposalCredentialValidFrom}
                onChange={(e) => setProposalCredentialValidFrom(e.target.value)}
                fullWidth
                slotProps={{
                  inputLabel: { shrink: true },
                }}
                help="When the minted credentials start to be valid. Leave empty for valid from the moment the action executes."
                helpLabel="Credentials Valid From"
              />
              <HelpTextField
                size="small"
                label="Credentials Valid Until (optional)"
                type="datetime-local"
                value={proposalCredentialValidUntil}
                onChange={(e) => setProposalCredentialValidUntil(e.target.value)}
                fullWidth
                slotProps={{
                  inputLabel: { shrink: true },
                }}
                help="When the minted credentials expire. Leave empty for credentials that never expire; ending those takes an Offboard Instrument Issuers vote. The proposal description shows this window to voters, and execution is refused once it has passed."
                helpLabel="Credentials Valid Until"
              />
            </>
          )}

          {proposalType === "offboard_instrument_issuers" && (
            <>
              <Typography variant="caption" color="text.secondary" sx={{ display: "block" }}>
                <TextHelp text="One row per offboarded issuer. Each row lists the credentials this governance party issued for that issuer. An issuer keeps minting rights through any credential left out.">
                  Instrument Issuers to Offboard (add issuer + credentials)
                </TextHelp>
              </Typography>
              {proposalOffboardRows.map((row, idx) => (
                <Box key={idx} sx={{ display: "flex", gap: 1, mb: 1, alignItems: "flex-start" }}>
                  <HelpTextField
                    label="Instrument Issuer Party"
                    value={row.party}
                    onChange={(e) => {
                      const party = e.target.value;
                      // Reset cids when the party changes. Stale
                      // credentials name the wrong subject and fail at
                      // execution.
                      const updated = [...proposalOffboardRows];
                      updated[idx] =
                        party === row.party ? row : { ...row, party, cids: [] };
                      setProposalOffboardRows(updated);
                    }}
                    size="small"
                    sx={{ flex: 2 }}
                    help="Party id of the issuer being offboarded. The credential list below filters to credentials whose claims all name this party."
                  />
                  <Autocomplete
                    size="small"
                    multiple
                    freeSolo
                    sx={{ flex: 3 }}
                    options={offboardableCredentialsFor(row.party)}
                    value={row.cids}
                    loading={credentialsLoading}
                    onChange={(_event, values) => {
                      const updated = [...proposalOffboardRows];
                      updated[idx] = {
                        ...row,
                        cids: values.map((value) =>
                          typeof value === "string" ? value : value.contract_id,
                        ),
                      };
                      setProposalOffboardRows(updated);
                    }}
                    getOptionLabel={(option) => {
                      const cid =
                        typeof option === "string" ? option : option.contract_id;
                      const known =
                        typeof option === "string"
                          ? availableCredentials.find((c) => c.contract_id === option)
                          : option;
                      if (!known) {
                        return cid;
                      }
                      return `${known.credential_id.split("/")[0]} (…${cid.slice(-8)})`;
                    }}
                    isOptionEqualToValue={(option, value) =>
                      typeof value === "string"
                        ? option.contract_id === value
                        : option.contract_id === value.contract_id
                    }
                    renderInput={(params) => (
                      <TextField
                        {...params}
                        label="Credentials to Revoke"
                        helperText={
                          credentialsLoading
                            ? "Loading credentials…"
                            : "Pick every credential of this issuer, or paste contract ids."
                        }
                      />
                    )}
                  />
                  <Button
                    size="small"
                    color="error"
                    onClick={() =>
                      setProposalOffboardRows(
                        proposalOffboardRows.filter((_, i) => i !== idx),
                      )
                    }
                  >
                    Remove
                  </Button>
                </Box>
              ))}
              <Box sx={{ display: "flex", alignItems: "center", gap: 2 }}>
                <Button
                  size="small"
                  onClick={() =>
                    setProposalOffboardRows([
                      ...proposalOffboardRows,
                      { party: "", cids: [] },
                    ])
                  }
                >
                  Add Issuer
                </Button>
              </Box>
            </>
          )}

          <SubmitButton
            label="Submit Proposal"
            loading={proposalLoading}
            disabled={
              proposalLoading ||
              beneficiaryProblem !== null ||
              blockListProblem !== null
            }
            onClick={handleSubmitProposal}
            portalEl={submitPortalEl}
          />
        </Box>
      </Collapse>
    </Box>
  );
};
