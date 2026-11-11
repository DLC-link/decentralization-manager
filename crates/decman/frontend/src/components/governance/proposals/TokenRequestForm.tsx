import { Autocomplete, Box, TextField, Typography } from "@mui/material";
import { TextHelp } from "../../FieldHelp";
import { ContractSelect } from "../fields/ContractSelect";
import { HelpTextField } from "../fields/HelpTextField";
import type { ProposalState } from "./useProposalState";
import type { TokenRequestInfo } from "../../../types";

interface Props {
  state: ProposalState;
}

/** The Accept Mint Request and Accept Burn Request form. */
export const TokenRequestForm = ({ state }: Props) => {
  const {
    proposalType,
    proposalDescription,
    setProposalDescription,
    proposalInstrumentConfigurationCid,
    setProposalInstrumentConfigurationCid,
    proposalMintRequestCid,
    setProposalMintRequestCid,
    proposalBurnRequestCid,
    setProposalBurnRequestCid,
    proposalIssuerCredentialCids,
    setProposalIssuerCredentialCids,
    proposalBlockListShardCids,
    setProposalBlockListShardCids,
    setBlockListShardsEdited,
    mintRequestContracts,
    mintRequestsLoading,
    burnRequestContracts,
    burnRequestsLoading,
    availableCredentials,
    credentialsLoading,
    blockListShardsLoading,
    availableInstruments,
    instrumentsLoading,
    acceptInstrument,
    acceptNeedsBlockListShards,
    registrarBlockListShards,
  } = state;

  return (
    <>
      {(proposalType === "accept_mint_request" || proposalType === "accept_burn_request") && (() => {
        const isMint = proposalType === "accept_mint_request";
        const requestContracts = isMint ? mintRequestContracts : burnRequestContracts;
        const requestCid = isMint ? proposalMintRequestCid : proposalBurnRequestCid;
        const setRequestCid = isMint ? setProposalMintRequestCid : setProposalBurnRequestCid;
        const requestLabel = isMint ? "MintRequest" : "BurnRequest";
        const requestsLoading = isMint ? mintRequestsLoading : burnRequestsLoading;
        // Mint = tokens created (green), burn = tokens destroyed (red).
        const accentColor = isMint ? "success.main" : "error.main";
        // Selecting a request prefills the matching InstrumentConfiguration
        // so the user doesn't have to re-pick the token by hand.
        const prefillInstrument = (req: TokenRequestInfo) => {
          const inst = availableInstruments.find(
            (i) =>
              i.instrument_admin === req.instrument_admin &&
              i.instrument_id === req.instrument_id,
          );
          if (!inst) return;
          setProposalInstrumentConfigurationCid(inst.contract_id);
          // The registrar's active shards divide all party hashes
          // between them, so together they cover every holder.
          setProposalBlockListShardCids(
            inst.configuration?.check_block_list === true
              ? registrarBlockListShards(inst.instrument_admin).map(
                  (shard) => shard.contract_id,
                )
              : [],
          );
          setBlockListShardsEdited(false);
        };
        const shardOptions = registrarBlockListShards(acceptInstrument?.instrument_admin);
        return (
          <>
            <Autocomplete
              size="small"
              freeSolo
              options={requestContracts}
              value={requestCid}
              loading={requestsLoading}
              onChange={(_event, value) => {
                if (typeof value === "string" || value === null) {
                  setRequestCid(value ?? "");
                } else {
                  setRequestCid(value.contract_id);
                  prefillInstrument(value);
                }
              }}
              onInputChange={(_event, value, reason) => {
                if (reason === "input") setRequestCid(value);
              }}
              getOptionLabel={(option) => {
                if (typeof option === "string") return option;
                const holderName = option.holder.split("::")[0];
                const amount = option.amount.replace(/\.?0+$/, "");
                const cidTail = option.contract_id.slice(-8);
                return `${holderName} → ${amount} ${option.instrument_id} (…${cidTail})`;
              }}
              getOptionDisabled={(option) => {
                if (typeof option === "string") return false;
                const exp = option.expires_at ?? 0;
                return exp > 0 && exp <= Math.floor(Date.now() / 1000);
              }}
              isOptionEqualToValue={(option, value) =>
                typeof value === "string"
                  ? option.contract_id === value
                  : option.contract_id === value.contract_id
              }
              renderOption={(props, option) => {
                if (typeof option === "string") {
                  return <li {...props}>{option}</li>;
                }
                const holderName = option.holder.split("::")[0];
                const amount = option.amount.replace(/\.?0+$/, "");
                const cidTail = option.contract_id.slice(-8);
                const exp = option.expires_at ?? 0;
                const isExpired =
                  exp > 0 && exp <= Math.floor(Date.now() / 1000);
                return (
                  <li {...props} key={option.contract_id}>
                    <Box
                      sx={{
                        display: "flex",
                        flexDirection: "column",
                        gap: 0.25,
                        opacity: isExpired ? 0.6 : 1,
                      }}
                    >
                      <Typography variant="body2">
                        {holderName} →{" "}
                        <Box
                          component="span"
                          sx={{ color: accentColor, fontWeight: 600 }}
                        >
                          {amount} {option.instrument_id}
                        </Box>{" "}
                        (…{cidTail})
                      </Typography>
                      {isExpired && (
                        <Typography variant="caption" color="warning.main">
                          Expired {new Date(exp * 1000).toLocaleString()}
                        </Typography>
                      )}
                    </Box>
                  </li>
                );
              }}
              renderInput={(params) => (
                <TextField
                  {...params}
                  label={
                    <TextHelp
                      text={
                        isMint
                          ? "MintRequest contract created by the holder that this proposal will accept."
                          : "BurnRequest contract created by the holder that this proposal will accept."
                      }
                    >
                      {requestLabel}
                    </TextHelp>
                  }
                  required
                  helperText={
                    requestsLoading
                      ? `Loading open ${isMint ? "mint" : "burn"} requests…`
                      : requestContracts.length === 0
                        ? `No ${requestLabel} contracts found — holder must create one first`
                        : "Pick an open request, or paste a contract id"
                  }
                />
              )}
            />
            <ContractSelect
              label="Instrument"
              help="Instrument the request was made against. Derived automatically from the selected request."
              value={proposalInstrumentConfigurationCid}
              onChange={setProposalInstrumentConfigurationCid}
              items={availableInstruments}
              loading={instrumentsLoading}
              loadingText="Loading instruments…"
              emptyText="No instruments found — run SetupUtility first"
              rowLabel={(inst) => <>{inst.instrument_id} ({inst.contract_id.slice(0, 8)}…)</>}
              disabled
            />
            <Autocomplete
              size="small"
              multiple
              freeSolo
              options={availableCredentials}
              value={proposalIssuerCredentialCids}
              loading={credentialsLoading}
              onChange={(_event, values) => {
                setProposalIssuerCredentialCids(
                  values.map((value) =>
                    typeof value === "string" ? value : value.contract_id,
                  ),
                );
              }}
              getOptionLabel={(option) => {
                // Selected values are stored as bare cids; label them
                // with the credential id when the contract is known.
                const cid = typeof option === "string" ? option : option.contract_id;
                const known =
                  typeof option === "string"
                    ? availableCredentials.find((c) => c.contract_id === option)
                    : option;
                const cidTail = cid.slice(-8);
                return known ? `${known.credential_id} (…${cidTail})` : cid;
              }}
              isOptionEqualToValue={(option, value) =>
                typeof value === "string"
                  ? option.contract_id === value
                  : option.contract_id === value.contract_id
              }
              renderOption={(props, option) => {
                const cidTail = option.contract_id.slice(-8);
                // The claims' subjects name the parties the credential
                // attests for — usually the request's holder.
                const subjects = [
                  ...new Set(option.claims.map((claim) => claim.subject.split("::")[0])),
                ].join(", ");
                return (
                  <li {...props} key={option.contract_id}>
                    <Box sx={{ display: "flex", flexDirection: "column", gap: 0.25 }}>
                      <Typography variant="body2">
                        {option.credential_id} (…{cidTail})
                      </Typography>
                      {subjects && (
                        <Typography variant="caption" color="text.secondary">
                          attests for {subjects}
                        </Typography>
                      )}
                    </Box>
                  </li>
                );
              }}
              renderInput={(params) => (
                <TextField
                  {...params}
                  label={
                    <TextHelp
                      text={
                        isMint
                          ? "Credentials proving the mint holder meets the instrument's issuer requirements. Leave empty for instruments without issuer requirements."
                          : "Credentials proving the burn holder meets the instrument's issuer requirements. Leave empty for instruments without issuer requirements."
                      }
                    >
                      Issuer Credentials
                    </TextHelp>
                  }
                  helperText={
                    credentialsLoading
                      ? "Loading credentials…"
                      : "Pick the holder's credentials, or paste contract ids. Optional for instruments without issuer requirements."
                  }
                />
              )}
            />
            {acceptNeedsBlockListShards && (
              <Autocomplete
                size="small"
                multiple
                freeSolo
                options={shardOptions}
                value={proposalBlockListShardCids}
                loading={blockListShardsLoading}
                onChange={(_event, values) => {
                  setBlockListShardsEdited(true);
                  setProposalBlockListShardCids(
                    values.map((value) =>
                      typeof value === "string" ? value : value.contract_id,
                    ),
                  );
                }}
                getOptionLabel={(option) => {
                  // Selected values are stored as bare cids; label them
                  // with the shard's prefix when the contract is known.
                  const cid = typeof option === "string" ? option : option.contract_id;
                  const known =
                    typeof option === "string"
                      ? shardOptions.find((s) => s.contract_id === option)
                      : option;
                  return known ? `Prefix ${known.prefix} (…${cid.slice(-8)})` : cid;
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
                      <TextHelp text="The registry checks the holder against these shards of the registrar's block list. Together, all of the registrar's shards cover every holder.">
                        Block List Shards
                      </TextHelp>
                    }
                    required
                    helperText={
                      blockListShardsLoading
                        ? "Loading block list shards…"
                        : shardOptions.length === 0
                          ? "No block list shards found for this registrar. Paste contract ids."
                          : "This instrument checks a block list. Keep all of the registrar's shards, or paste contract ids."
                    }
                  />
                )}
              />
            )}
            <HelpTextField
              size="small"
              label="Description"
              value={proposalDescription}
              onChange={(e) => setProposalDescription(e.target.value)}
              fullWidth
              required
              help="Free-form human-readable note explaining why this request is being accepted."
            />
          </>
        );
      })()}
    </>
  );
};
