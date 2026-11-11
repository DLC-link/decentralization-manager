import { useCallback } from "react";
import { Alert, Autocomplete, Box, Button, TextField, Typography } from "@mui/material";
import { TextHelp } from "../../FieldHelp";
import { ContractSelect } from "../fields/ContractSelect";
import { HelpTextField } from "../fields/HelpTextField";
import { RequirementRows, type RequirementRow } from "../fields/RequirementRows";
import { requiredClaimsText } from "../proposalParse";
import type { ProposalState } from "./useProposalState";
import type { PartyCredentialRequirement } from "../../../types";

interface Props {
  state: ProposalState;
}

/**
 * The Provision Instrument, Onboard and Offboard Instrument Issuers, Create
 * Transfer Rule and Update Instrument Configuration forms.
 */
export const InstrumentForms = ({ state }: Props) => {
  const {
    partyId,
    proposalType,
    proposalCredentialValidFrom,
    setProposalCredentialValidFrom,
    proposalCredentialValidUntil,
    setProposalCredentialValidUntil,
    proposalInstrumentIdText,
    setProposalInstrumentIdText,
    proposalAdditionalIdentifiers,
    setProposalAdditionalIdentifiers,
    proposalInstrumentConfigurationCid,
    setProposalInstrumentConfigurationCid,
    proposalRegistrarServiceCid,
    setProposalRegistrarServiceCid,
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
    availableCredentials,
    credentialsLoading,
    registrarServicesLoading,
    instrumentsLoading,
    ownRegistrarServices,
    ownInstruments,
    configurationToUpdateUnreadable,
    blockListProblem,
  } = state;

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

  return (
    <>
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
    </>
  );
};
