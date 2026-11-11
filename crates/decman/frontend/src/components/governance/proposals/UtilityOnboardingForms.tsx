import {
  Alert,
  Checkbox,
  FormControl,
  FormControlLabel,
  InputLabel,
  MenuItem,
  Select,
} from "@mui/material";
import { TextHelp } from "../../FieldHelp";
import { ContractSelect } from "../fields/ContractSelect";
import { HelpTextField } from "../fields/HelpTextField";
import { AmuletRulesCidField } from "./AmuletRulesCidField";
import type { ProposalState } from "./useProposalState";
import type { Network } from "../../../types";

interface Props {
  state: ProposalState;
  network?: Network;
}

/** The utility onboarding forms: service requests, Setup Utility and utility settings. */
export const UtilityOnboardingForms = ({ state, network }: Props) => {
  const {
    proposalType,
    proposalProvider,
    setProposalProvider,
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
    proposalUser,
    setProposalUser,
    proposalRegistrarServiceCid,
    setProposalRegistrarServiceCid,
    proposalEnableResultContracts,
    setProposalEnableResultContracts,
    providerServices,
    userServices,
    registrarServiceContracts,
    servicesLoading,
  } = state;

  return (
    <>
      {proposalType === "create_provider_service_request" &&
        (network === "testnet" || network === "mainnet") && (
          <Alert severity="info">
            On TestNet and MainNet, a credential from Digital Asset (DA)
            is required before a Provider Service Request can be
            accepted.
          </Alert>
        )}

      {proposalType === "setup_utility" && (
        <>
          <ContractSelect
            label="ProviderService"
            help="ProviderService contract this party received from the operator. Required to set up the registrar."
            value={proposalProviderServiceCid}
            onChange={setProposalProviderServiceCid}
            items={providerServices}
            loading={servicesLoading}
            loadingText="Loading services…"
            emptyText='No ProviderService found — run "Create Provider Service Request" first'
          />
          <HelpTextField
            size="small"
            label="Operator Party"
            value={proposalOperator}
            onChange={(e) => setProposalOperator(e.target.value)}
            fullWidth
            required
            help="Party id of the utility operator that issued the ProviderService."
          />
          <HelpTextField
            size="small"
            label="Instrument ID"
            value={proposalInstrumentIdText}
            onChange={(e) => setProposalInstrumentIdText(e.target.value)}
            fullWidth
            required
            help={"Token name for the instrument this utility will mint and burn (e.g. \"cTM\"). The governance party is the issuer."}
          />
          <FormControlLabel
            control={<Checkbox size="small" checked={proposalCreateTransferRule} onChange={(e) => setProposalCreateTransferRule(e.target.checked)} />}
            label={
              <TextHelp text="Also create a TransferRule contract so holders can transfer this token without per-transfer governance.">
                Create TransferRule
              </TextHelp>
            }
          />
          <FormControlLabel
            control={<Checkbox size="small" checked={proposalCreateAllocationFactory} onChange={(e) => setProposalCreateAllocationFactory(e.target.checked)} />}
            label={
              <TextHelp text="Also create an AllocationFactory contract so this token can be allocated by external apps.">
                Create AllocationFactory
              </TextHelp>
            }
          />
        </>
      )}

      {proposalType === "create_provider_service_request" && (
        <>
          {providerServices.length > 0 && (
            <Alert severity="warning">
              This party already has {providerServices.length} ProviderService
              contract{providerServices.length === 1 ? "" : "s"}; creating
              another request will fail when executed.
            </Alert>
          )}
          <HelpTextField
            size="small"
            label="Operator Party"
            value={proposalOperator}
            onChange={(e) => setProposalOperator(e.target.value)}
            fullWidth
            required
            help="Party id of the utility operator that will receive and sign off on this request."
          />
          <HelpTextField
            size="small"
            label="Provider Party"
            value={proposalProvider}
            onChange={(e) => setProposalProvider(e.target.value)}
            fullWidth
            required
            help="Party id that wants to become a provider on the utility (usually this governance party)."
          />
        </>
      )}

      {proposalType === "create_user_service_request" && (
        <>
          {userServices.length > 0 && (
            <Alert severity="warning">
              This party already has {userServices.length} UserService
              contract{userServices.length === 1 ? "" : "s"}; creating
              another request will fail when executed.
            </Alert>
          )}
          <HelpTextField
            size="small"
            label="Operator Party"
            value={proposalOperator}
            onChange={(e) => setProposalOperator(e.target.value)}
            fullWidth
            required
            help="Party id of the utility operator that will receive and sign off on this request."
          />
          <HelpTextField
            size="small"
            label="User Party"
            value={proposalUser}
            onChange={(e) => setProposalUser(e.target.value)}
            fullWidth
            required
            help="Party id that wants to become a user of the utility (usually this governance party)."
          />
        </>
      )}

      {proposalType === "set_enable_result_contracts" && (
        <>
          <ContractSelect
            label="RegistrarService"
            help="RegistrarService contract whose result-contract setting will be updated."
            value={proposalRegistrarServiceCid}
            onChange={setProposalRegistrarServiceCid}
            items={registrarServiceContracts}
            emptyText="No RegistrarService found — run SetupUtility first"
          />
          <FormControl size="small" fullWidth>
            <InputLabel>
              <TextHelp text="Whether the registrar should emit result contracts after operations. Clear sets the value back to None.">
                Enable Result Contracts
              </TextHelp>
            </InputLabel>
            <Select
              label="Enable Result Contracts"
              value={proposalEnableResultContracts}
              onChange={(e) => setProposalEnableResultContracts(e.target.value as "true" | "false" | "clear")}
            >
              <MenuItem value="true">Enable</MenuItem>
              <MenuItem value="false">Disable</MenuItem>
              <MenuItem value="clear">Clear (None)</MenuItem>
            </Select>
          </FormControl>
        </>
      )}

      {proposalType === "create_delegated_batched_markers_proxy" && (
        <HelpTextField
          size="small"
          label="Operator Party"
          value={proposalOperator}
          onChange={(e) => setProposalOperator(e.target.value)}
          fullWidth
          required
          help="Party id of the utility operator that will own the delegated batched markers proxy."
        />
      )}

      {proposalType === "request_dev_net_featured_app_right" && (
        <AmuletRulesCidField state={state} />
      )}
    </>
  );
};
